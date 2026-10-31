use std::{io::Result, sync::Arc, time::Duration};

use tokio::{
    net::TcpListener,
    sync::{Mutex, mpsc},
    time::sleep,
};

use crate::{
    node::{
        peer::handler::{handle_handshake, handle_peer_connection},
        runner::deduplicate_peer_connection,
        state::{Node, PeerId, PeerSocket},
    },
    types::message::ReplicaInBound,
};

async fn drop_peer_socket(node: Arc<Node>, peer_id: PeerId) {
    let logger = node.logger.clone();
    {
        let mut peer_connections = node.peer_connections.write().await;
        peer_connections.remove(&peer_id);
    }
    logger.log(
        "Info",
        &format!("Dropping peer connection to peer: {:?}", peer_id),
    );
}

/// peer listener handles the consensus layer communication
pub(crate) async fn run_peer_listener(
    node: Arc<Node>,
    concensus_addr: String,
    // peer: PeerInfo,
    to_replica_tx: mpsc::Sender<ReplicaInBound>,
) -> Result<()> {
    let peer_listener: TcpListener = TcpListener::bind(&concensus_addr).await?;
    let logger = node.logger.clone();
    logger.log(
        "Info",
        &format!("Listening to peers on {:?}", concensus_addr),
    );

    loop {
        let (stream, _) = match peer_listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                logger.log(
                    "Error",
                    &format!(
                        "Failed to accept after binding to :{:?}: {:?}",
                        concensus_addr, e
                    ),
                );
                sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let tx_clone = to_replica_tx.clone();
        let node_clone = node.clone();
        logger.log("Info", "Spawning peer listener");
        let logger = logger.clone();

        let (reader, writer) = stream.into_split();
        let writer = Arc::new(Mutex::new(writer));
        let reader = Arc::new(Mutex::new(reader));

        let peer_socket = Arc::new(PeerSocket::new(reader, writer));

        tokio::spawn(async move {
            // TODO: (Dos) no handshake timeout or cap on concurrent connections,
            // so idle or slow connections can accumulate.
            let peer_id = match handle_handshake(
                peer_socket.reader.clone(),
                peer_socket.writer.clone(),
                logger.clone(),
            )
            .await
            {
                Ok(id) => id,
                Err(e) => {
                    logger.log("Error", &format!("Handshake failed: {:?}", e));
                    return;
                }
            };

            let peer_socket = deduplicate_peer_connection(peer_socket, &node_clone, peer_id).await;
            match handle_peer_connection(&node_clone, peer_socket.reader.clone(), tx_clone).await {
                Ok(()) => logger.log("info", "Successfully handled peer connection"),
                Err(e) => logger.log("Error", &format!("Peer listener: Failed due to: {:?}", e)),
            }

            // drop the peerSocket after the connection has ended
            drop_peer_socket(node_clone, peer_id).await;
        });
    }
}
