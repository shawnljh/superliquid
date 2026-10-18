use ed25519::signature::SignerMut;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    block::{Block, BlockHash},
    crypto::{PartialSig, QuorumCertificate, QuorumCertificateHash},
};
use crate::{
    node::state::PeerId,
    types::{consensus::ViewNumber, transaction::Sha256Hash},
};

pub struct UnsignedVote {
    block_hash: BlockHash,
    view: ViewNumber,
}

impl UnsignedVote {
    pub fn new(view: ViewNumber, block_hash: BlockHash) -> Self {
        Self { block_hash, view }
    }

    pub fn hash(&self) -> Sha256Hash {
        let hashable = HashableMessage {
            view_number: self.view,
            block_hash: self.block_hash,
            quorum_hash: QuorumCertificateHash::default(),
        };

        let encoded = bincode::serialize(&hashable).unwrap();
        Sha256::digest(&encoded).into()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Proposal,
    Vote,
    NewView,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct HotStuffEnvelope {
    pub from: PeerId,
    pub msg: HotStuffMessage,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum HotStuffMessage {
    Proposal {
        node: Block,
        view: ViewNumber,
        sender_view: ViewNumber,
    },
    Vote {
        block_hash: BlockHash,
        partial_sig: PartialSig,
        view: ViewNumber,
        sender_view: ViewNumber,
    },
    NewView {
        justify: QuorumCertificate,
        view: ViewNumber,
        sender_view: ViewNumber,
    },
}

#[derive(Serialize, Deserialize)]
pub struct HashableMessage {
    view_number: ViewNumber,
    block_hash: BlockHash,
    quorum_hash: QuorumCertificateHash,
}

impl HotStuffMessage {
    pub fn create_new_view(
        justify: QuorumCertificate,
        view: ViewNumber,
        sender_view: ViewNumber,
    ) -> Self {
        Self::NewView {
            justify,
            view,
            sender_view,
        }
    }

    pub fn create_proposal(node: Block, view: ViewNumber, sender_view: ViewNumber) -> Self {
        Self::Proposal {
            node,
            view,
            sender_view,
        }
    }

    pub fn create_vote(
        block_hash: BlockHash,
        view: ViewNumber,
        sender_view: ViewNumber,
        signing_key: &mut SigningKey,
    ) -> Self {
        let unsigned_vote = UnsignedVote { block_hash, view };

        let signed_vote = unsigned_vote.hash();
        let signature = signing_key.sign(&signed_vote);

        let partial_sig = PartialSig {
            signer_id: signing_key.verifying_key(),
            signature,
        };

        HotStuffMessage::Vote {
            block_hash,
            partial_sig,
            view,
            sender_view,
        }
    }

    pub fn hash(&self) -> Sha256Hash {
        let hashable = match self {
            HotStuffMessage::Proposal { node, view, .. } => HashableMessage {
                view_number: *view,
                block_hash: node.hash(),
                quorum_hash: QuorumCertificateHash::default(),
            },
            HotStuffMessage::Vote {
                view, block_hash, ..
            } => HashableMessage {
                view_number: *view,
                block_hash: *block_hash,
                quorum_hash: QuorumCertificateHash::default(),
            },
            HotStuffMessage::NewView { justify, view, .. } => HashableMessage {
                view_number: *view,
                block_hash: BlockHash::default(),
                quorum_hash: justify.hash(),
            },
        };

        let encoded = bincode::serialize(&hashable).unwrap();
        Sha256::digest(&encoded).into()
    }

    pub fn get_view_number(&self) -> ViewNumber {
        let (HotStuffMessage::Proposal { view, .. }
        | HotStuffMessage::Vote { view, .. }
        | HotStuffMessage::NewView { view, .. }) = self;

        *view
    }

    pub fn get_sender_view(&self) -> ViewNumber {
        let (HotStuffMessage::Proposal { sender_view, .. }
        | HotStuffMessage::Vote { sender_view, .. }
        | HotStuffMessage::NewView { sender_view, .. }) = self;
        *sender_view
    }

    pub(crate) fn get_message_qc(&self) -> Option<&QuorumCertificate> {
        match self {
            HotStuffMessage::Proposal { node, .. } => {
                let (Block::Genesis { justify, .. } | Block::Normal { justify, .. }) = node;
                Some(justify)
            }
            HotStuffMessage::NewView { justify, .. } => Some(justify),
            _ => None,
        }
    }
}
