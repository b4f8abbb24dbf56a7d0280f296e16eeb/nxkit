//! A bounded single-producer single-consumer queue that never blocks and never allocates after creation.

#![allow(unsafe_code)]

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The queue. One thread pushes and one thread pops. Both may be different threads over time as long as the pushes (or the
/// pops) are serialised by the caller.
pub struct Ring<T> {
    slots: Box<[UnsafeCell<T>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: a slot is written by the producer before `tail` moves past it and read by the consumer before `head` moves past
// it, so no slot is accessed by both sides at once.
unsafe impl<T: Send> Sync for Ring<T> {}
// SAFETY: the contents are `Send` and the indices are atomics.
unsafe impl<T: Send> Send for Ring<T> {}

impl<T: Copy + Default> Ring<T> {
    /// A queue that holds up to `capacity` values.
    pub fn new(capacity: usize) -> Ring<T> {
        let n = capacity + 1;
        let slots: Vec<UnsafeCell<T>> = (0..n).map(|_| UnsafeCell::new(T::default())).collect();
        Ring {
            slots: slots.into_boxed_slice(),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Adds a value. Returns false when the queue is full.
    pub fn push(&self, v: T) -> bool {
        let n = self.slots.len();
        let tail = self.tail.load(Ordering::Relaxed);
        let next = (tail + 1) % n;
        if next == self.head.load(Ordering::Acquire) {
            return false;
        }
        // SAFETY: the slot at `tail` is free, so the consumer does not read it until `tail` moves.
        unsafe { *self.slots[tail].get() = v };
        self.tail.store(next, Ordering::Release);
        true
    }

    /// Removes the oldest value.
    pub fn pop(&self) -> Option<T> {
        let n = self.slots.len();
        let head = self.head.load(Ordering::Relaxed);
        if head == self.tail.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: the slot at `head` is full, so the producer does not write it until `head` moves.
        let v = unsafe { *self.slots[head].get() };
        self.head.store((head + 1) % n, Ordering::Release);
        Some(v)
    }
}
