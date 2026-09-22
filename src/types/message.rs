use std::io;

use serde::{Deserialize, Serialize};

use crate::{
    hotstuff::message::HotStuffMessage,
    networking::message_protocol::{AppMessage, ControlMessage},
    node::{client::handler::QueryRequest, state::PeerId},
};

use super::transaction::SignedTransaction;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    Connection(ControlMessage),
    Application(AppMessage),
    HotStuff(HotStuffMessage),
}

pub enum ReplicaOutbound {
    Broadcast(HotStuffMessage),
    SendTo(PeerId, HotStuffMessage),
}

pub enum ReplicaInBound {
    HotStuff(HotStuffMessage),
    Transaction(SignedTransaction),
    Query(QueryRequest),
}

pub fn mpsc_error<E: std::fmt::Display>(context: &str, err: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{}: {}", context, err))
}
