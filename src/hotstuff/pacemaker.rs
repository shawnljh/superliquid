use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{config, pacemaker_log, types::consensus::ViewNumber};

use super::crypto::QuorumCertificate;

pub struct Pacemaker {
    pub curr_view: ViewNumber,
    pub last_commited_view: ViewNumber,
    pub timeout: Duration,
    pub base_timeout: Duration,
    pub timeout_multiplicative_factor: f32,
    pub last_view_change: Instant,
    replica_ids: Vec<usize>,
    // pub highest_qc: QuorumCertificate,
    // pub leaf: Block,
}

impl Pacemaker {
    pub(crate) fn new() -> Self {
        let replica_len = config::retrieve_num_validators();
        let replica_ids = (0..replica_len).collect();
        Self {
            curr_view: 0,
            last_commited_view: 0,
            timeout: config::retrieve_tick_duration(),
            timeout_multiplicative_factor: config::retrieve_multiplicative_factor(),

            base_timeout: config::retrieve_tick_duration(),
            last_view_change: Instant::now(),
            replica_ids,
        }
    }

    pub(crate) fn should_advance_view(&self) -> bool {
        self.last_view_change.elapsed() > self.get_current_timeout()
    }

    pub(crate) fn get_current_timeout(&self) -> Duration {
        let failed_views = self.curr_view - self.last_commited_view;

        self.base_timeout
            * self
                .timeout_multiplicative_factor
                .powf(failed_views as f32)
                .round() as u32
    }

    pub(crate) fn set_last_committed_view(&mut self, qc: Arc<QuorumCertificate>) {
        self.last_commited_view = qc.view_number;
    }

    pub(crate) fn advance_view(&mut self) {
        pacemaker_log!(
            "Timeout occured - advancing view from {:?} to {:?}",
            self.curr_view,
            self.curr_view + 1
        );
        self.curr_view += 1;
        self.last_view_change = Instant::now();
    }

    pub(crate) fn fast_forward_view(&mut self, incoming_view: ViewNumber) -> bool {
        if incoming_view <= self.curr_view {
            return false;
        }
        pacemaker_log!(
            "Fast forwarding view from {:?} to {:?}",
            self.curr_view,
            incoming_view
        );
        self.curr_view = incoming_view;
        self.last_view_change = Instant::now();
        true
    }

    pub(crate) fn current_leader(&self) -> usize {
        self.replica_ids[(self.curr_view as usize) % self.replica_ids.len()]
    }

    pub(crate) fn get_leader_for_view(&self, incoming_view: ViewNumber) -> usize {
        self.replica_ids[(incoming_view as usize) % self.replica_ids.len()]
    }

    pub(crate) fn reset_timer(&mut self) {
        self.last_view_change = Instant::now();
    }

    pub(crate) fn time_remaining(&self) -> Duration {
        let end_time = self.last_view_change + self.get_current_timeout();
        end_time.saturating_duration_since(Instant::now())
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_new_pacemaker_starts_at_view_zero() {
        let pacemaker = Pacemaker::new();
        assert_eq!(pacemaker.curr_view, 0);
        assert_eq!(
            pacemaker.replica_ids.len(),
            config::retrieve_num_validators()
        );
    }

    #[test]
    fn test_should_advance_view_false_initially() {
        let pacemaker = Pacemaker::new();
        assert!(!pacemaker.should_advance_view());
    }

    #[test]
    fn test_should_advance_view_after_timeout() {
        let pacemaker = Pacemaker::new();
        // simulate passage of time
        sleep(pacemaker.timeout + std::time::Duration::from_millis(10));
        assert!(pacemaker.should_advance_view());
    }

    #[test]
    fn test_advance_view_increments_view_and_resets_timer() {
        let mut pacemaker = Pacemaker::new();
        let initial_time = pacemaker.last_view_change;
        pacemaker.advance_view();
        assert_eq!(pacemaker.curr_view, 1);
        assert!(pacemaker.last_view_change > initial_time);
    }

    #[test]
    fn test_set_view_updates_view_and_resets_timer() {
        let mut pacemaker = Pacemaker::new();

        pacemaker.curr_view = 5;
        let before = pacemaker.last_view_change;

        std::thread::sleep(Duration::from_millis(10)); // Let time pass

        pacemaker.fast_forward_view(10); // Should update view and reset timer

        assert_eq!(pacemaker.curr_view, 10);
        assert!(pacemaker.last_view_change > before);
    }

    #[test]
    fn test_set_view_does_not_regress_or_reset_timer() {
        let mut pacemaker = Pacemaker::new();

        pacemaker.curr_view = 8;
        let before = pacemaker.last_view_change;

        std::thread::sleep(Duration::from_millis(10)); // Let time pass

        pacemaker.fast_forward_view(6); // Should NOT update or reset

        assert_eq!(pacemaker.curr_view, 8);
        assert_eq!(pacemaker.last_view_change, before);
    }

    #[test]
    fn test_get_current_timeout_exponential_backoff() {
        let mut pacemaker = Pacemaker::new();
        pacemaker.base_timeout = Duration::from_millis(10);

        pacemaker.curr_view = 3;
        pacemaker.last_commited_view = 1;
        // 3 - 1 = 2 => 10ms * 1.5^2 = 10ms * 2.2 = 22ms
        let expected = Duration::from_millis(20);
        assert_eq!(pacemaker.get_current_timeout(), expected);
    }

    #[test]
    fn test_current_leader_rotates_among_replicas() {
        let mut pacemaker = Pacemaker::new();
        let total_replicas = pacemaker.replica_ids.len();

        for i in 0..total_replicas * 2 {
            pacemaker.curr_view = i as u64;
            let expected_leader = pacemaker.replica_ids[i % total_replicas];
            assert_eq!(pacemaker.current_leader(), expected_leader);
        }
    }

    #[test]
    fn test_time_remaining_decreases() {
        let pacemaker = Pacemaker::new();
        let t1 = pacemaker.time_remaining();
        sleep(std::time::Duration::from_millis(10));
        let t2 = pacemaker.time_remaining();
        assert!(t2 < t1);
    }

    #[test]
    fn test_should_advance_view() {
        let mut pacemaker = Pacemaker::new();

        // Override timeout to make the test fast
        pacemaker.base_timeout = Duration::from_millis(20);
        pacemaker.curr_view = 2;
        pacemaker.last_commited_view = 0;

        // Initially, it should not advance (timeout hasn't passed)
        assert!(!pacemaker.should_advance_view());

        // Wait long enough to exceed the exponential backoff timeout
        // Timeout = base * 2^(2 - 0) = 20ms * 4 = 80ms
        std::thread::sleep(Duration::from_millis(85));

        assert!(pacemaker.should_advance_view());
    }
}
