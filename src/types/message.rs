use std::io;

use serde::{Deserialize, Serialize};

use crate::{
    hotstuff::message::HotStuffEnvelope,
    networking::message_protocol::{AppMessage, ControlMessage},
    node::{client::handler::QueryRequest, state::PeerId},
};

use super::transaction::SignedTransaction;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    Connection(ControlMessage),
    Application(AppMessage),
    HotStuff(HotStuffEnvelope),
}

pub enum ReplicaOutbound {
    Broadcast(HotStuffEnvelope),
    SendTo(PeerId, HotStuffEnvelope),
}

pub enum ReplicaInBound {
    HotStuff(HotStuffEnvelope),
    Transaction(SignedTransaction),
    Query(QueryRequest),
}

pub fn mpsc_error<E: std::fmt::Display>(context: &str, err: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{}: {}", context, err))
}
