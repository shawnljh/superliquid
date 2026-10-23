//! Peer connections are assumed to be mutually authenticated and
//! integrity-protected. The envelope's `from` is trusted on that basis.
//! Not yet implemented: the current hello handshake is unauthenticated.
pub mod runner;
pub mod state;

pub(crate) mod client;
pub(crate) mod peer;
pub(crate) mod replica;

pub mod logger;
