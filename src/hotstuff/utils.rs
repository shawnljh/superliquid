use std::num::NonZeroUsize;

use crate::types::consensus::ViewNumber;

use super::{crypto::QuorumCertificate, message::HotStuffMessage};

use ed25519::Signature;
use hex::{decode as hex_decode, encode as hex_encode};

#[derive(Debug)]
pub(crate) struct ConsensusBudget {
    curr_budget: usize,
    default: usize,
}

impl ConsensusBudget {
    pub(crate) fn new(consensus_budget: NonZeroUsize) -> Self {
        Self {
            curr_budget: consensus_budget.get(),
            default: consensus_budget.get(),
        }
    }

    pub(crate) fn is_exhausted(&self) -> bool {
        self.curr_budget == 0
    }

    pub(crate) fn refill(&mut self) {
        self.curr_budget = self.default;
    }

    pub(crate) fn consume(&mut self) -> bool {
        if self.is_exhausted() {
            return false;
        }
        self.curr_budget = self.curr_budget.saturating_sub(1);
        true
    }
}

pub fn get_highest_qc_from_votes<'a>(
    votes: &Vec<&'a HotStuffMessage>,
) -> Option<&'a QuorumCertificate> {
    votes
        .iter()
        .filter_map(|msg| match msg {
            HotStuffMessage::NewView { justify, .. } => Some(justify),
            _ => None,
        })
        .max_by_key(|&qc| qc.view_number)
}

pub(crate) fn has_quorum_votes_for_view(
    messages: Option<Vec<&HotStuffMessage>>,
    curr_view: ViewNumber,
    quorum_threhold: usize,
) -> bool {
    let Some(msgs) = messages else {
        return false;
    };

    msgs.iter()
        .filter(|m| matches!(m, HotStuffMessage::Vote { view, .. } if *view == curr_view))
        .count()
        >= quorum_threhold
}

pub(crate) fn sig_to_string(sig: &Signature) -> String {
    hex_encode(sig.to_bytes()) // outputs lowercase hex
}

pub(crate) fn string_to_sig(s: &str) -> Result<Signature, &'static str> {
    let bytes = hex_decode(s).map_err(|_| "Invalid hex")?;
    let array: [u8; 64] = bytes.try_into().map_err(|_| "Invalid sig length")?;
    Ok(Signature::from_bytes(&array))
}
