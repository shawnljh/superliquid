//! MessageWindow stores recent Consensus messages in a sliding window indexed by view number.
//! Within each view, messages are grouped by their phase (newview, proposal) and are keyed by
//! ReplicaID.
//!
//! # Considerations
//! 1. ViewNumber of inbound messages can be arbitrarily large (a byzantine node).
//! 2. Each replica can and should have different kinds of messages in each view (different
//!    Phases/ timeout)
//!
//! # Invariants
//!   - Given P number of phases in each view and N replicas, per view messages will be bounded
//!     by N x P.
//!   - MAX_VIEW_LOOKAHEAD bounds the size of the vector to guard against an arbitrarily
//!     large ViewNumber sent by a byzantine node. Given a MAX_VIEW_LOOKAHEAD = L, any ViewNumber
//!     \>= lowest_view + L will be rejected. Importantly, resynchronization is not a responsibility
//!     of this module. Given MAX_VIEW_LOOKAHEAD = L, the size of
//!     the overall datastructure is L x N x P.
//!
//! # Design Goals
//! - Efficiently store and retrieve messages grouped by view.
//! - Support fast pruning of outdated views as consensus advances.
//! - Maximize memory locality and cache efficiency.
//! - Handle gaps between views without requiring strict continuity.
//!
//! # Data Structures
//! - struct ViewSlot:
//!     - Contains maps for each phase, which each map being keyed by ReplicaID
//!
//! - messages: `Vec<ViewSlot>`:
//!   - `Vec` holds messages for consecutive views, starting from `lowest_view`.
//!     chosen over Vecdeque in optimistic cases, views is small (< 5) and will be drained.
//!     Although pruning the vector is an O(V) operation, where V is the number of views,
//!     the runtime should be negligible. Bounded by `MAX_VIEW_LOOKAHEAD`
//!
//! - `lowest_view`: `ViewNumber`:
//!   - Lowest relevant view that should be retained by the replica. Retention logic
//!     is driven by the protocol.
//!
//! - `MAX_VIEW_LOOKAHEAD`: Bounds the maximum view from the lowest_view

use std::collections::HashMap;

use crate::{
    hotstuff::message::Phase,
    types::consensus::{ReplicaId, ViewNumber},
};

use super::message::HotStuffMessage;

#[derive(Debug, PartialEq, Default)]
pub(crate) struct ViewSlot {
    pub new_view: HashMap<ReplicaId, HotStuffMessage>,
    pub propose: HashMap<ReplicaId, HotStuffMessage>,
    pub vote: HashMap<ReplicaId, HotStuffMessage>,
    pub epoch: HashMap<ReplicaId, HotStuffMessage>,
    pub prepare: HashMap<ReplicaId, HotStuffMessage>,
}

pub struct MessageWindow {
    messages: Vec<ViewSlot>,
    lowest_view: ViewNumber,
    max_view_lookahead: usize,
}

impl MessageWindow {
    pub fn new(view_number: ViewNumber, max_view_lookahead: usize) -> Self {
        MessageWindow {
            messages: Vec::with_capacity(max_view_lookahead),
            lowest_view: view_number,
            max_view_lookahead,
        }
    }
    // No check on whether this view will be needed
    pub fn prune_before_view(&mut self, view: ViewNumber) {
        if view < self.lowest_view {
            // no messages to prune
            return;
        }

        if (view as usize) > (self.lowest_view as usize) + self.messages.len() {
            // prune all current messages
            // PERF: Possible optimsation/profile: drain vs reallocating
            self.messages.drain(0..self.messages.len());
            self.lowest_view = view;
            return;
        }

        let to_remove = view - self.lowest_view;

        self.messages.drain(0..to_remove as usize);

        self.lowest_view = view;
    }

    pub fn push(&mut self, msg: HotStuffMessage) -> bool {
        if msg.get_view_number() < self.lowest_view {
            return false;
        }

        if msg.get_view_number() >= self.lowest_view + (self.max_view_lookahead as u64) {
            return false;
        }

        let index = (msg.get_view_number() - self.lowest_view) as usize;
        if index >= self.messages.len() {
            self.messages.resize_with(index + 1, ViewSlot::default);
        }
        let view_slot = &mut self.messages[index];
        let sender = msg.get_sender();
        let map = match msg {
            HotStuffMessage::Proposal { .. } => &mut view_slot.propose,
            HotStuffMessage::Vote { .. } => &mut view_slot.vote,
            HotStuffMessage::NewView { .. } => &mut view_slot.new_view,
        };
        map.insert(sender, msg);
        true
    }

    fn get_viewslot_for_view(&self, view: ViewNumber) -> Option<&ViewSlot> {
        if view < self.lowest_view {
            return None;
        }

        let highest_view = self.lowest_view as usize + self.messages.len();
        if (view as usize) > highest_view {
            return None;
        }

        let index = (view - self.lowest_view) as usize;
        self.messages.get(index)
    }

    pub fn get_phase_messages_for_view(
        &self,
        view: ViewNumber,
        phase: Phase,
    ) -> Option<Vec<&HotStuffMessage>> {
        let viewslot = self.get_viewslot_for_view(view)?;
        match phase {
            Phase::NewView => Some(viewslot.new_view.values().collect()),
            Phase::Proposal => Some(viewslot.propose.values().collect()),
            Phase::Vote => Some(viewslot.vote.values().collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        hotstuff::{
            block::Block,
            crypto::{PartialSig, QuorumCertificate},
        },
        node::state::PeerId,
    };

    use super::*;

    fn dummy_qc() -> QuorumCertificate {
        QuorumCertificate::mock(0)
    }

    fn dummy_block() -> Block {
        Block::mock(0)
    }

    fn dummy_partial_sig() -> PartialSig {
        PartialSig::mock(0)
    }

    fn dummy_new_view_message(view: ViewNumber, sender: PeerId) -> HotStuffMessage {
        HotStuffMessage::create_new_view(dummy_qc(), view, sender, 0)
    }

    fn dummy_proposal_message(view: ViewNumber, sender: PeerId) -> HotStuffMessage {
        HotStuffMessage::Proposal {
            node: dummy_block(),
            view,
            sender,
            sender_view: view,
        }
    }

    fn dummy_vote_message(view: ViewNumber, sender: PeerId) -> HotStuffMessage {
        HotStuffMessage::Vote {
            node: dummy_block(),
            partial_sig: dummy_partial_sig(),
            view,
            sender,
            sender_view: view,
        }
    }

    // ---- new ----

    #[test]
    fn new_initializes_empty_window() {
        let window = MessageWindow::new(5, 10);
        assert_eq!(window.messages.len(), 0);
        assert_eq!(window.lowest_view, 5);
        assert_eq!(window.max_view_lookahead, 10);
    }

    // ---- push: routing by phase ----

    #[test]
    fn push_proposal_routes_to_propose_map() {
        let mut window = MessageWindow::new(5, 10);
        let msg = dummy_proposal_message(5, 1);
        assert!(window.push(msg.clone()));

        let slot = window.get_viewslot_for_view(5).unwrap();
        assert_eq!(slot.propose.get(&1), Some(&msg));
        assert!(slot.vote.is_empty());
        assert!(slot.new_view.is_empty());
    }

    #[test]
    fn push_vote_routes_to_vote_map() {
        let mut window = MessageWindow::new(5, 10);
        let msg = dummy_vote_message(5, 1);
        assert!(window.push(msg.clone()));

        let slot = window.get_viewslot_for_view(5).unwrap();
        assert_eq!(slot.vote.get(&1), Some(&msg));
        assert!(slot.propose.is_empty());
        assert!(slot.new_view.is_empty());
    }

    #[test]
    fn push_new_view_routes_to_new_view_map() {
        let mut window = MessageWindow::new(5, 10);
        let msg = dummy_new_view_message(5, 1);
        assert!(window.push(msg.clone()));

        let slot = window.get_viewslot_for_view(5).unwrap();
        assert_eq!(slot.new_view.get(&1), Some(&msg));
        assert!(slot.propose.is_empty());
        assert!(slot.vote.is_empty());
    }

    // ---- push: view-range boundaries ----

    #[test]
    fn push_rejects_view_below_lowest() {
        let mut window = MessageWindow::new(5, 10);
        assert!(!window.push(dummy_new_view_message(4, 1)));
        assert!(window.messages.is_empty());
    }

    #[test]
    fn push_accepts_view_equal_to_lowest() {
        let mut window = MessageWindow::new(5, 10);
        assert!(window.push(dummy_new_view_message(5, 1)));
        assert_eq!(window.messages.len(), 1);
    }

    #[test]
    fn push_rejects_view_at_upper_boundary() {
        let mut window = MessageWindow::new(5, 10);
        assert!(!window.push(dummy_new_view_message(15, 1)));
        assert!(window.messages.is_empty());
    }

    #[test]
    fn push_accepts_view_just_below_upper_boundary() {
        let mut window = MessageWindow::new(5, 10);
        assert!(window.push(dummy_new_view_message(14, 1)));
        assert_eq!(window.messages.len(), 10);
    }

    #[test]
    fn push_grows_vec_to_cover_future_view() {
        let mut window = MessageWindow::new(3, 10);
        let msg = dummy_new_view_message(6, 1);
        assert!(window.push(msg.clone()));
        assert_eq!(window.messages.len(), 4); // views 3,4,5,6
        assert_eq!(
            window.get_viewslot_for_view(6).unwrap().new_view.get(&1),
            Some(&msg)
        );
        assert!(window.get_viewslot_for_view(4).unwrap().new_view.is_empty());
    }

    #[test]
    fn push_duplicate_sender_same_view_overwrites() {
        let mut window = MessageWindow::new(5, 10);
        window.push(dummy_new_view_message(5, 1));
        let msg2 = dummy_new_view_message(5, 1);
        window.push(msg2.clone());

        let slot = window.get_viewslot_for_view(5).unwrap();
        assert_eq!(slot.new_view.len(), 1);
        assert_eq!(slot.new_view.get(&1), Some(&msg2));
    }

    #[test]
    fn push_different_senders_same_view_both_kept() {
        let mut window = MessageWindow::new(5, 10);
        window.push(dummy_new_view_message(5, 1));
        window.push(dummy_new_view_message(5, 2));

        let slot = window.get_viewslot_for_view(5).unwrap();
        assert_eq!(slot.new_view.len(), 2);
    }

    // ---- get_viewslot_for_view ----

    #[test]
    fn get_viewslot_for_view_none_below_lowest() {
        let window = MessageWindow::new(5, 10);
        assert!(window.get_viewslot_for_view(4).is_none());
    }

    #[test]
    fn get_viewslot_for_view_none_when_out_of_range() {
        let mut window = MessageWindow::new(5, 10);
        window.push(dummy_new_view_message(5, 1));
        assert!(window.get_viewslot_for_view(6).is_none());
    }

    #[test]
    fn get_viewslot_for_view_some_within_range() {
        let mut window = MessageWindow::new(5, 10);
        window.push(dummy_new_view_message(7, 1));
        assert!(window.get_viewslot_for_view(5).is_some());
        assert!(window.get_viewslot_for_view(6).is_some());
        assert!(window.get_viewslot_for_view(7).is_some());
    }

    // ---- get_phase_messages_for_view ----

    #[test]
    fn get_phase_messages_for_view_none_out_of_range() {
        let window = MessageWindow::new(5, 10);
        assert!(
            window
                .get_phase_messages_for_view(4, Phase::NewView)
                .is_none()
        );
    }

    #[test]
    fn get_phase_messages_for_view_returns_matching_phase_only() {
        let mut window = MessageWindow::new(5, 10);
        window.push(dummy_new_view_message(5, 1));
        window.push(dummy_vote_message(5, 2));
        window.push(dummy_proposal_message(5, 3));

        let new_view_msgs = window
            .get_phase_messages_for_view(5, Phase::NewView)
            .unwrap();
        assert_eq!(new_view_msgs.len(), 1);

        let vote_msgs = window.get_phase_messages_for_view(5, Phase::Vote).unwrap();
        assert_eq!(vote_msgs.len(), 1);

        let proposal_msgs = window
            .get_phase_messages_for_view(5, Phase::Proposal)
            .unwrap();
        assert_eq!(proposal_msgs.len(), 1);
    }

    #[test]
    fn get_phase_messages_for_view_empty_map_returns_empty_vec() {
        let mut window = MessageWindow::new(5, 10);
        window.push(dummy_new_view_message(5, 1));

        // vote map is empty for this view, but view itself is in range -> Some(vec![])
        let vote_msgs = window.get_phase_messages_for_view(5, Phase::Vote).unwrap();
        assert!(vote_msgs.is_empty());
    }

    #[test]
    fn get_phase_messages_for_view_multiple_senders() {
        let mut window = MessageWindow::new(5, 10);
        window.push(dummy_vote_message(5, 1));
        window.push(dummy_vote_message(5, 2));
        window.push(dummy_vote_message(5, 3));

        let vote_msgs = window.get_phase_messages_for_view(5, Phase::Vote).unwrap();
        assert_eq!(vote_msgs.len(), 3);
    }

    // ---- prune_before_view ----

    #[test]
    fn prune_before_view_noop_when_below_lowest() {
        let mut window = MessageWindow::new(5, 10);
        window.push(dummy_new_view_message(5, 1));
        window.prune_before_view(4);
        assert_eq!(window.lowest_view, 5);
        assert_eq!(window.messages.len(), 1);
    }

    #[test]
    fn prune_before_view_removes_prefix() {
        let mut window = MessageWindow::new(2, 10);
        for v in 2..6 {
            window.push(dummy_new_view_message(v, 1));
        }
        window.prune_before_view(4);
        assert_eq!(window.lowest_view, 4);
        assert_eq!(window.messages.len(), 2); // views 4 and 5 remain
        assert_eq!(
            window
                .get_viewslot_for_view(4)
                .unwrap()
                .new_view
                .get(&1)
                .unwrap()
                .get_view_number(),
            4
        );
    }

    #[test]
    fn prune_before_view_clears_all_when_beyond_window() {
        let mut window = MessageWindow::new(1, 10);
        window.push(dummy_new_view_message(1, 1));
        window.push(dummy_new_view_message(2, 2));
        window.prune_before_view(10);
        assert_eq!(window.messages.len(), 0);
        assert_eq!(window.lowest_view, 10);
    }

    #[test]
    fn prune_before_view_exact_upper_boundary_clears_all() {
        let mut window = MessageWindow::new(2, 10);
        window.push(dummy_new_view_message(2, 1));
        window.push(dummy_new_view_message(3, 1));
        window.prune_before_view(4);
        assert_eq!(window.messages.len(), 0);
        assert_eq!(window.lowest_view, 4);
    }

    #[test]
    fn push_after_prune_uses_new_lowest_view() {
        let mut window = MessageWindow::new(2, 10);
        window.push(dummy_new_view_message(2, 1));
        window.prune_before_view(5);
        assert!(window.push(dummy_new_view_message(5, 1)));
        assert!(window.get_viewslot_for_view(5).is_some());
        assert!(!window.push(dummy_new_view_message(4, 1)));
    }
}
