use futures::future::join_all;
use tokio::{
    net::TcpStream,
    sync::{Mutex, RwLock, mpsc},
    time::sleep,
};

use crate::{
    config,
    hotstuff::replica::HotStuffReplica,
    networking::message_protocol::send_hello,
    types::message::{ReplicaInBound, ReplicaOutbound},
};

use super::{
    client::listener::run_client_listener,
    logger::ConsoleLogger,
    peer::listener::run_peer_listener,
    replica::handle_replica_outbound,
    state::{Node, PeerId, PeerInfo, PeerSocket},
};
use std::{
    collections::{HashMap, HashSet},
    io::Result,
    sync::Arc,
    time,
};

async fn spawn_all_node_tasks(
    client_addr: String,
    consensus_addr: String,
    mut replica: HotStuffReplica,
    node: Arc<Node>,
    to_replica_tx: mpsc::Sender<ReplicaInBound>,
    to_replica_rx: mpsc::Receiver<ReplicaInBound>,
    from_replica_rx: mpsc::Receiver<ReplicaOutbound>,
) -> Result<()> {
    let handles = vec![
        tokio::spawn(run_client_listener(
            client_addr.to_owned(),
            node.clone(),
            to_replica_tx.clone(),
        )),
        tokio::spawn(run_peer_listener(
            node.clone(),
            consensus_addr.to_owned(),
            to_replica_tx.clone(),
        )),
    ];

    tokio::spawn(async move { replica.run_replica(to_replica_rx).await });
    tokio::spawn(handle_replica_outbound(from_replica_rx, node.clone()));

    let _ = join_all(handles).await;
    Ok(())
}

// It is possible for two nodes to establish connections with one another,
// Without  Deduplication, we can have conflicting streams for the same peer.
pub(crate) async fn deduplicate_peer_connection(
    stream: Arc<PeerSocket>,
    node: &Arc<Node>,
    peer_id: PeerId,
) -> Arc<PeerSocket> {
    let logger = node.logger.clone();
    let mut peer_connections = node.peer_connections.write().await;
    match peer_connections.get(&peer_id) {
        Some(stream) => {
            logger.log(
                "Info",
                &format!("Deduplicated TCP stream with peer: {:?}", peer_id),
            );
            stream.clone()
        }
        None => {
            let stream_clone = stream.clone();
            peer_connections.insert(peer_id, stream_clone);
            stream.clone()
        }
    }
}

pub(crate) async fn connect_to_peer(addr: String, peer_id: usize, node: Arc<Node>) {
    let base: u32 = 100;
    let mut counts: u32 = 1;
    let max_sleep: u32 = 1000 * 60;
    let logger = node.logger.clone();
    loop {
        match TcpStream::connect(addr.clone()).await {
            Ok(stream) => {
                logger.log(
                    "info",
                    &format!("Initiate: Connection established with peer {}", peer_id),
                );
                let socket_addr = stream
                    .peer_addr()
                    .expect("Expect stream to have peer address");

                let (reader, writer) = stream.into_split();

                {
                    let mut socket_peer_map = node.socket_peer_map.write().await;
                    socket_peer_map.entry(socket_addr).or_insert(peer_id);
                }
                let reader = Arc::new(Mutex::new(reader));
                let writer = Arc::new(Mutex::new(writer));

                let peer_socket = Arc::new(PeerSocket::new(reader, writer));

                send_hello(
                    peer_socket.writer.clone(),
                    peer_socket.reader.clone(),
                    node.id,
                )
                .await
                .unwrap();

                let _ = deduplicate_peer_connection(peer_socket, &node, peer_id).await;
                break;
            }
            Err(e) => {
                logger.log("error", &format!("Failed to connect to {}: {:?}", addr, e));
                let exp_duration = base.pow(counts);
                let sleep_duration = exp_duration.min(max_sleep);
                if sleep_duration != max_sleep {
                    counts = counts.saturating_add(1);
                }

                sleep(time::Duration::from_millis(sleep_duration.into())).await;
            }
        }
    }
}

async fn connect_to_peers_background(peers: &Vec<PeerInfo>, node: &Arc<Node>) {
    for peer_info in peers {
        let node_clone = node.clone();
        let peer_id = peer_info.peer_id;
        let addr = peer_info.peer_addr.clone();
        tokio::spawn(async move {
            connect_to_peer(addr, peer_id, node_clone).await;
        });
    }
}

pub async fn run_node(
    client_addr: String,
    consensus_addr: String,
    peers: Vec<PeerInfo>,
    node_index: usize,
) -> Result<()> {
    // Bind the listener to the address

    let peers = Arc::new(peers);
    let (_, sk) = config::retrieve_faucet_keys();
    let node = Arc::new(Node {
        id: node_index,
        faucet_key: sk,
        transactions: Mutex::new(vec![]),
        seen_transactions: Mutex::new(HashSet::new()),
        peer_connections: RwLock::new(HashMap::new()),
        logger: Arc::new(ConsoleLogger::new(node_index)),
        socket_peer_map: RwLock::new(HashMap::new()),
    });
    connect_to_peers_background(&peers, &node).await;

    // Sends messages to replica from node
    let (to_replica_tx, to_replica_rx): (
        mpsc::Sender<ReplicaInBound>,
        mpsc::Receiver<ReplicaInBound>,
    ) = mpsc::channel(1024);

    // Recieves messages from replica to node
    let (from_replica_tx, from_replica_rx): (
        mpsc::Sender<ReplicaOutbound>,
        mpsc::Receiver<ReplicaOutbound>,
    ) = mpsc::channel(1024);

    let replica = HotStuffReplica::new(node_index, to_replica_tx.clone(), from_replica_tx);

    let _ = spawn_all_node_tasks(
        client_addr,
        consensus_addr,
        replica,
        node,
        to_replica_tx,
        to_replica_rx,
        from_replica_rx,
    )
    .await;
    Ok(())
}
