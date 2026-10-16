use std::cell::UnsafeCell;
use std::sync::atomic::{
    AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};

use crossbeam_utils::CachePadded;
use std::mem::MaybeUninit;
use std::sync::Arc;

use crate::types::common::QueueCapacity;

// Send is auto-derived since Arc<SPSCLFQueue> is Send + Sync
// when T is Send
pub(crate) struct SPSCLFQueueSender<T> {
    queue: Arc<SPSCLFQueue<T>>,
}

impl<T> SPSCLFQueueSender<T> {
    pub(crate) fn try_enqueue(&mut self, item: T) -> Result<(), T> {
        self.queue.enqueue(item)
    }
}

pub(crate) struct SPSCLFQueueReceiver<T> {
    queue: Arc<SPSCLFQueue<T>>,
}

impl<T> SPSCLFQueueReceiver<T> {
    pub(crate) fn try_recv(&mut self) -> Result<T, ()> {
        self.queue.dequeue()
    }
}

struct SPSCLFQueue<T> {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    cap: usize,
    mask: usize,
    arr: Vec<UnsafeCell<MaybeUninit<T>>>,
}

// Safety:
// Sync only works if and only if only one thread calls enqueue and only one thread calls dequeue
//
//
// enqueue and dequeue are private, each only reachable through a non-clone Sender and
// non-Clone receiver, each requiring &mut self to call the methods
//
// This interface design ensures that one thread can call enqueue and one thread can call dequeue
unsafe impl<T> Sync for SPSCLFQueue<T> where T: Send {}

pub(crate) fn spsc_lf_queue<T>(
    queue_cap: QueueCapacity,
) -> (SPSCLFQueueSender<T>, SPSCLFQueueReceiver<T>) {
    let queue = Arc::new(SPSCLFQueue::new(queue_cap));

    (
        SPSCLFQueueSender {
            queue: queue.clone(),
        },
        SPSCLFQueueReceiver { queue },
    )
}

impl<T> SPSCLFQueue<T> {
    fn new(queue_cap: QueueCapacity) -> Self {
        let cap = queue_cap.get();
        let mut arr: Vec<UnsafeCell<MaybeUninit<T>>> = Vec::new();
        arr.resize_with(cap, || UnsafeCell::new(MaybeUninit::uninit()));

        Self {
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
            arr,
            cap,
            mask: cap - 1,
        }
    }
    fn is_empty(&self) -> bool {
        // Is_empty is primarily used to check whether its safe to dequeue
        // Acquire load on head establishes a happens before relationship with the release
        // in enqueue.
        // Relaxed ordering on tail is okay since there will only be one thread modifying tail
        let head = self.head.load(Acquire);
        let tail = self.tail.load(Relaxed);

        head.wrapping_sub(tail) == 0
    }

    fn is_full(&self) -> bool {
        // is_full is primarily used to check whether its safe to enqueue
        // Acquire load on tail establishes a happens before relationship with the release in
        // dequeue
        // Relaxed ordering on head is okay since there will only be one thread modifying head
        let head = self.head.load(Relaxed);
        let tail = self.tail.load(Acquire);

        head.wrapping_sub(tail) >= self.cap
    }

    fn enqueue(&self, item: T) -> Result<(), T> {
        if self.is_full() {
            return Err(item);
        }

        let mut head = self.head.load(Relaxed);
        let index = head & self.mask;

        // SAFETY:
        // why get_unchecked is safe: index is always be within bounds since arr is initalised to equal `cap`
        // length and the index is masked by into [0, cap).
        //
        // why exclusive access to the MaybeUninit is safe:
        // 1. enqueue will only be used by 1 producer at anytime,
        // in that situation getting a &mut to the MaybeUninit is safe
        // 2. Value within the MaybeUninit should be safe to overwrite since head.wrapping_sub(tail)
        // < cap. The consumer will always read within the bounds of [tail, head)
        // and a happens before relationship is established with the release on tail with the
        // Acquire on is_full
        unsafe {
            (&mut *self.arr.get_unchecked(index).get()).write(item);
        }
        head = head.wrapping_add(1);
        // Establishes a happens before with acquire on is_empty
        self.head.store(head, Release);
        Ok(())
    }

    fn dequeue(&self) -> Result<T, ()> {
        if self.is_empty() {
            return Err(());
        }

        let mut tail = self.tail.load(Relaxed);
        let index = tail & self.mask;

        let item;
        // SAFETY:
        // Why get_unchecked is safe:
        // index is always be within bounds since arr is initalised to equal
        // `cap` length and the index is masked to [0, cap).
        //
        // Why assume_init_read is safe:
        // 1. dequeue will only be used by 1 consumer at anytime, in that situation, we can get mut access
        // to the index and the value can be moved out to the caller as it will not be read twice
        // 2. Value within the MaybeUninit are initalised as producer will have written to them.
        // Is_empty establishes a "happens before relationship" with the release on head
        // ensuring that the value is written since index is [tail, head)
        unsafe {
            let x = &mut *self.arr.get_unchecked(index).get();
            item = x.assume_init_read();
        }

        tail = tail.wrapping_add(1);

        // Establishes a happens before with acquire on is_full
        self.tail.store(tail, Release);
        Ok(item)
    }
}

impl<T> Drop for SPSCLFQueue<T> {
    fn drop(&mut self) {
        let tail = *self.tail.get_mut();
        let head = *self.head.get_mut();
        let mut i = tail;
        while i != head {
            let index = i & self.mask;
            // SAFETY:
            // Why get_unchecked_mut is safe:
            // index is always within bounds because the array is initialised to `cap`
            // length and the index is masked by cap - 1 means index will be in  the bounds of
            // [0, cap).
            //
            // Why assume_init_drop is safe:
            // 1. No double free: The loop will visit head.wrapped_sub(tail) values which
            // is at most cap, and the values are mod cap. In this case, each value can only
            // be visited at most 1 time.
            // 2. No dropping uninit values: We have exclusive access on head and tail
            // meaning that we see all writes to them. Any value between [tail, head)
            // must have been initialised since enqueue wrote to them.
            unsafe {
                self.arr
                    .get_unchecked_mut(index)
                    .get_mut()
                    .assume_init_drop();
            }
            i = i.wrapping_add(1);
        }
    }
}

// TODO: add loom tests to exhaustively verify the head/tail acquire/release orderings.// TODO: add loom tests to exhaustively verify the head/tail acquire/release orderings.
#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn queue<T>(exponent: u32) -> (SPSCLFQueueSender<T>, SPSCLFQueueReceiver<T>) {
        spsc_lf_queue(QueueCapacity::new(exponent))
    }

    /// Starts head and tail just below usize::MAX so the counters overflow mid-test.
    fn queue_near_overflow<T>(exponent: u32) -> (SPSCLFQueueSender<T>, SPSCLFQueueReceiver<T>) {
        let q = SPSCLFQueue::new(QueueCapacity::new(exponent));
        let start = usize::MAX - 2;
        q.head.store(start, Relaxed);
        q.tail.store(start, Relaxed);
        let q = Arc::new(q);
        (
            SPSCLFQueueSender { queue: q.clone() },
            SPSCLFQueueReceiver { queue: q },
        )
    }

    /// Increments a shared counter when dropped, to check each item is dropped exactly once.
    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Relaxed);
        }
    }

    // ---- single-threaded behaviour ----

    #[test]
    fn new_queue_is_empty() {
        let (_tx, mut rx) = queue::<u64>(2);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn single_item_round_trip() {
        let (mut tx, mut rx) = queue(2);
        assert!(tx.try_enqueue(42u64).is_ok());
        assert_eq!(rx.try_recv(), Ok(42));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn preserves_fifo_order() {
        let (mut tx, mut rx) = queue(3); // cap 8
        for i in 0..8u64 {
            tx.try_enqueue(i).unwrap();
        }
        for i in 0..8u64 {
            assert_eq!(rx.try_recv(), Ok(i));
        }
    }

    #[test]
    fn full_queue_returns_item_to_caller() {
        let (mut tx, mut rx) = queue(2); // cap 4
        for i in 0..4u64 {
            tx.try_enqueue(i).unwrap();
        }
        assert_eq!(tx.try_enqueue(99), Err(99));

        // freeing one slot makes room again
        assert_eq!(rx.try_recv(), Ok(0));
        assert!(tx.try_enqueue(99).is_ok());
    }

    #[test]
    fn capacity_one() {
        let (mut tx, mut rx) = queue(0); // cap 1, mask 0
        tx.try_enqueue(1u64).unwrap();
        assert_eq!(tx.try_enqueue(2), Err(2));
        assert_eq!(rx.try_recv(), Ok(1));
        assert!(rx.try_recv().is_err());
        tx.try_enqueue(3).unwrap();
        assert_eq!(rx.try_recv(), Ok(3));
    }

    #[test]
    fn wraps_around_many_times() {
        let (mut tx, mut rx) = queue(2); // cap 4
        for round in 0..100u64 {
            for i in 0..3 {
                tx.try_enqueue(round * 3 + i).unwrap();
            }
            for i in 0..3 {
                assert_eq!(rx.try_recv(), Ok(round * 3 + i));
            }
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn counters_survive_usize_overflow() {
        let (mut tx, mut rx) = queue_near_overflow(2); // cap 4

        // head wraps past usize::MAX during these pushes while tail has not
        for i in 0..4u64 {
            tx.try_enqueue(i).unwrap();
        }
        assert_eq!(tx.try_enqueue(4), Err(4));
        for i in 0..4u64 {
            assert_eq!(rx.try_recv(), Ok(i));
        }
        assert!(rx.try_recv().is_err());

        // both counters have now wrapped
        for i in 10..14u64 {
            tx.try_enqueue(i).unwrap();
        }
        for i in 10..14u64 {
            assert_eq!(rx.try_recv(), Ok(i));
        }
    }

    // ---- Drop ----

    #[test]
    fn drop_empty_queue_drops_nothing() {
        let drops = Arc::new(AtomicUsize::new(0));
        {
            let (mut tx, mut rx) = queue(2);
            assert!(tx.try_enqueue(DropCounter(drops.clone())).is_ok());
            drop(rx.try_recv());
            assert_eq!(drops.load(Relaxed), 1);
        }
        assert_eq!(drops.load(Relaxed), 1);
    }

    #[test]
    fn drop_releases_remaining_items_exactly_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        {
            let (mut tx, mut rx) = queue(2);
            for _ in 0..3 {
                assert!(tx.try_enqueue(DropCounter(drops.clone())).is_ok());
            }
            drop(rx.try_recv()); // caller drops the dequeued item
            assert_eq!(drops.load(Relaxed), 1);
        } // queue dropped with two items still inside
        assert_eq!(drops.load(Relaxed), 3);
    }

    #[test]
    fn drop_full_queue_after_wraparound() {
        let drops = Arc::new(AtomicUsize::new(0));
        {
            let (mut tx, mut rx) = queue(2); // cap 4

            // advance tail to 6 so head and tail are mid-array
            for _ in 0..6 {
                assert!(tx.try_enqueue(DropCounter(drops.clone())).is_ok());
                drop(rx.try_recv());
            }
            assert_eq!(drops.load(Relaxed), 6);

            // fill completely: head & mask == tail & mask
            for _ in 0..4 {
                assert!(tx.try_enqueue(DropCounter(drops.clone())).is_ok());
            }

            // the rejected item comes back to the caller, not into the queue
            let rejected = tx.try_enqueue(DropCounter(drops.clone()));
            assert!(rejected.is_err());
            drop(rejected);
            assert_eq!(drops.load(Relaxed), 7);
        }
        assert_eq!(drops.load(Relaxed), 11);
    }

    // ---- concurrency ----

    const N: u64 = if cfg!(miri) { 1_000 } else { 100_000 };

    #[test]
    fn concurrent_producer_consumer_preserves_order() {
        let (mut tx, mut rx) = queue(4); // cap 16, small to hit full and empty often

        let producer = thread::spawn(move || {
            for i in 0..N {
                let mut item = i;
                while let Err(back) = tx.try_enqueue(item) {
                    item = back;
                    std::hint::spin_loop();
                }
            }
        });

        let consumer = thread::spawn(move || {
            let mut expected = 0;
            while expected < N {
                match rx.try_recv() {
                    Ok(v) => {
                        assert_eq!(v, expected);
                        expected += 1;
                    }
                    Err(()) => std::hint::spin_loop(),
                }
            }
        });

        producer.join().unwrap();
        consumer.join().unwrap();
    }

    #[test]
    fn concurrent_heap_values_transfer_ownership() {
        // Box exercises the drop/ownership paths; under miri this catches use-after-free
        let (mut tx, mut rx) = queue(4);

        let producer = thread::spawn(move || {
            for i in 0..N {
                let mut item = Box::new(i);
                while let Err(back) = tx.try_enqueue(item) {
                    item = back;
                    std::hint::spin_loop();
                }
            }
        });

        let consumer = thread::spawn(move || {
            let mut expected = 0;
            while expected < N {
                if let Ok(v) = rx.try_recv() {
                    assert_eq!(*v, expected);
                    expected += 1;
                } else {
                    std::hint::spin_loop();
                }
            }
        });

        producer.join().unwrap();
        consumer.join().unwrap();
    }

    #[test]
    fn handles_are_send() {
        fn assert_send<X: Send>() {}
        assert_send::<SPSCLFQueueSender<u64>>();
        assert_send::<SPSCLFQueueReceiver<Box<u64>>>();
    }
}
