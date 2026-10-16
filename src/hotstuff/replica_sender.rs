use tokio::sync::mpsc;

use crate::types::message::{ReplicaInBound, ReplicaOutbound, mpsc_error};

use super::message::HotStuffMessage;

pub struct ReplicaSender {
    pub replica_tx: mpsc::Sender<ReplicaInBound>,
    pub node_tx: mpsc::Sender<ReplicaOutbound>,
}

impl ReplicaSender {
    pub(super) async fn broadcast(&self, msg: HotStuffMessage) -> Result<(), std::io::Error> {
        self.node_tx
            .send(ReplicaOutbound::Broadcast(msg))
            .await
            .map_err(|e| mpsc_error("failed to send to node", e))
    }

    pub(super) async fn send_to_node(
        &self,
        node_id: usize,
        msg: HotStuffMessage,
    ) -> Result<(), std::io::Error> {
        self.node_tx
            .send(ReplicaOutbound::SendTo(node_id, msg))
            .await
            .map_err(|e| mpsc_error("failed to send to node", e))
    }
}
