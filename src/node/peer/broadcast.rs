use std::{io::Result, sync::Arc};

use futures::future::join_all;

use crate::{
    hotstuff::message::HotStuffMessage,
    networking::message_protocol::{send_message, send_transaction},
    node::state::{Node, PeerId},
    types::{message::Message, transaction::SignedTransaction},
};

/// Broadcast msg to all peer connections
pub(crate) async fn broadcast_hotstuff_message(
    node: &Arc<Node>,
    msg: HotStuffMessage,
) -> Result<()> {
    let peer_connections = node.get_peer_connections_as_vec().await;

    for peer_socket in peer_connections {
        let cloned_msg = msg.clone();
        tokio::spawn(async move {
            send_message(peer_socket.writer.clone(), &Message::HotStuff(cloned_msg)).await
        });
    }

    Ok(())
}

pub(crate) async fn send_to_peer(
    node: &Arc<Node>,
    msg: HotStuffMessage,
    peer_id: PeerId,
) -> Result<()> {
    let peer_connection = {
        let peer_connections = node.peer_connections.read().await;
        peer_connections.get(&peer_id).cloned()
    };

    let Some(peer_connection) = peer_connection else {
        return Ok(());
    };
    send_message(peer_connection.writer.clone(), &Message::HotStuff(msg)).await
}

pub(crate) async fn broadcast_transaction(node: &Arc<Node>, tx: SignedTransaction) -> Result<()> {
    let id = node.id;
    let logger = &node.logger.clone();

    let peer_connections = node.get_peer_connections_as_vec().await;

    let mut tasks = Vec::new();

    logger.log(
        "info",
        &format!(
            "broadcasting tx from node {} to {} peers",
            id,
            peer_connections.len()
        ),
    );

    for peer_socket in peer_connections {
        let cloned_tx = tx.clone();
        let task =
            tokio::spawn(
                async move { send_transaction(peer_socket.writer.clone(), cloned_tx).await },
            );
        tasks.push(task);
    }

    let results = join_all(tasks).await;
    for result in results {
        match result {
            Ok(Ok(())) => {
                logger.log("info", "sent transaction ");
            }
            Ok(Err(e)) => logger.log("Error", &format!("send_transaction error: {:?}", e)),
            Err(e) => logger.log("Error", &format!("task panicked: {:?}", e)),
        }
    }

    logger.log("info", "Finish broadcasting tx");
    Ok(())
}
