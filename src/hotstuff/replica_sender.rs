use tokio::sync::mpsc;

use crate::{
    node::state::PeerId,
    types::message::{ReplicaInBound, ReplicaOutbound, mpsc_error},
};

use super::message::{HotStuffEnvelope, HotStuffMessage};

pub struct ReplicaSender {
    node_id: PeerId,
    pub replica_tx: mpsc::Sender<ReplicaInBound>,
    pub node_tx: mpsc::Sender<ReplicaOutbound>,
}

impl ReplicaSender {
    pub(super) fn new(
        node_id: PeerId,
        replica_tx: mpsc::Sender<ReplicaInBound>,
        node_tx: mpsc::Sender<ReplicaOutbound>,
    ) -> Self {
        Self {
            node_id,
            replica_tx,
            node_tx,
        }
    }

    pub(super) async fn broadcast(&self, msg: HotStuffMessage) -> Result<(), std::io::Error> {
        let envelop = HotStuffEnvelope {
            from: self.node_id,
            msg,
        };

        self.node_tx
            .send(ReplicaOutbound::Broadcast(envelop))
            .await
            .map_err(|e| mpsc_error("failed to send to node", e))
    }

    pub(super) async fn send_to_node(
        &self,
        node_id: usize,
        msg: HotStuffMessage,
    ) -> Result<(), std::io::Error> {
        let envelop = HotStuffEnvelope {
            from: self.node_id,
            msg,
        };

        self.node_tx
            .send(ReplicaOutbound::SendTo(node_id, envelop))
            .await
            .map_err(|e| mpsc_error("failed to send to node", e))
    }
}
