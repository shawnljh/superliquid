use std::cell::UnsafeCell;
use std::sync::atomic::{
    AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};

use std::mem::MaybeUninit;
use std::sync::Arc;

use crate::types::common::QueueCapacity;

pub struct Sender<T> {
    queue: Arc<SPSCLFQueue<T>>,
}

pub struct Reciever<T> {
    queue: Arc<SPSCLFQueue<T>>,
}

pub struct SPSCLFQueue<T> {
    head: AtomicUsize,
    tail: AtomicUsize,
    cap: usize,
    mask: usize,
    arr: Vec<UnsafeCell<MaybeUninit<T>>>,
}

unsafe impl<T> Sync for SPSCLFQueue<T> where T: Send {}

impl<T> SPSCLFQueue<T> {
    fn new(queue_cap: QueueCapacity) -> Self {
        let cap = queue_cap.get();
        let mut arr: Vec<UnsafeCell<MaybeUninit<T>>> = Vec::new();
        arr.resize_with(cap, || UnsafeCell::new(MaybeUninit::uninit()));

        Self {
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
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

    fn enqueue(&self, item: T) -> bool {
        if self.is_full() {
            return false;
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
        true
    }

    fn dequeue(&self) -> Option<T> {
        if self.is_empty() {
            return None;
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
        Some(item)
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
