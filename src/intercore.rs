//! Bounded single-producer/single-consumer ownership transfer without a heap.
//!
//! On P4 both endpoints and the queue must use coherent memory (initially the
//! same uncached SRAM alias). Release/Acquire do NOT write back CPU caches or
//! make cached payload buffers coherent. A notification is only a doorbell:
//! queues carry the state and may contain multiple messages for one interrupt.
use core::{
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::MaybeUninit,
    sync::atomic::{AtomicUsize, Ordering},
};

#[repr(align(64))]
struct Cursor(AtomicUsize);

/// Owns any values still queued when dropped. N must be a power of two >= 2.
/// All N slots are usable. Endpoints cannot be cloned or shared concurrently.
#[repr(C, align(64))]
pub struct Queue<T, const N: usize> {
    produced: Cursor,
    consumed: Cursor,
    slots: [UnsafeCell<MaybeUninit<T>>; N],
}
// SAFETY: split requires exclusive access. Only the producer writes a slot,
// only the consumer reads it, and Acquire/Release delimit its live interval.
// Cross-thread movement of a payload requires Send; T need not be Sync.
unsafe impl<T: Send, const N: usize> Sync for Queue<T, N> {}

impl<T, const N: usize> Default for Queue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T, const N: usize> Queue<T, N> {
    pub const fn new() -> Self {
        assert!(N >= 2 && N.is_power_of_two());
        Self {
            produced: Cursor(AtomicUsize::new(0)),
            consumed: Cursor(AtomicUsize::new(0)),
            slots: [const { UnsafeCell::new(MaybeUninit::uninit()) }; N],
        }
    }
    /// A new pair can only be obtained after both previous endpoints are gone.
    /// Dropping an endpoint does not discard or reset queued values.
    pub fn split(&mut self) -> (Producer<'_, T, N>, Consumer<'_, T, N>) {
        (
            Producer {
                queue: self,
                _single: PhantomData,
            },
            Consumer {
                queue: self,
                _single: PhantomData,
            },
        )
    }
}
impl<T, const N: usize> Drop for Queue<T, N> {
    fn drop(&mut self) {
        let mut next = *self.consumed.0.get_mut();
        let end = *self.produced.0.get_mut();
        while next != end {
            // SAFETY: no endpoints survive this exclusive borrow; this slot is live.
            unsafe {
                self.slots[next & (N - 1)].get_mut().assume_init_drop();
            }
            next = next.wrapping_add(1);
        }
    }
}

pub struct Producer<'a, T, const N: usize> {
    queue: &'a Queue<T, N>,
    _single: PhantomData<Cell<()>>,
}
pub struct Consumer<'a, T, const N: usize> {
    queue: &'a Queue<T, N>,
    _single: PhantomData<Cell<()>>,
}
impl<T, const N: usize> Producer<'_, T, N> {
    /// Full returns the original value; ownership stays with the caller.
    pub fn try_push(&mut self, value: T) -> Result<(), T> {
        let q = self.queue;
        let tail = q.produced.0.load(Ordering::Relaxed);
        let head = q.consumed.0.load(Ordering::Acquire);
        if tail.wrapping_sub(head) == N {
            return Err(value);
        }
        // SAFETY: head publication guarantees this producer-owned slot is empty.
        unsafe {
            (*q.slots[tail & (N - 1)].get()).write(value);
        }
        q.produced.0.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }
}
impl<T, const N: usize> Consumer<'_, T, N> {
    /// Snapshot for a notification-aware idle predicate. The producer may
    /// publish immediately after this read; callers must handle that race.
    pub fn is_empty(&self) -> bool {
        self.queue.consumed.0.load(Ordering::Relaxed)
            == self.queue.produced.0.load(Ordering::Acquire)
    }
    pub fn try_pop(&mut self) -> Option<T> {
        let q = self.queue;
        let head = q.consumed.0.load(Ordering::Relaxed);
        let tail = q.produced.0.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        // SAFETY: producer publication guarantees one initialized value; only
        // this endpoint reads it, before returning the empty slot to producer.
        let value = unsafe { (*q.slots[head & (N - 1)].get()).assume_init_read() };
        q.consumed.0.store(head.wrapping_add(1), Ordering::Release);
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        thread,
    };
    struct Owned(Arc<AtomicUsize>);
    impl Drop for Owned {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn full_queue_returns_ownership_and_drop_reclaims_remaining_values() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut q = Queue::<Owned, 2>::new();
        {
            let (mut tx, mut rx) = q.split();
            assert!(tx.try_push(Owned(drops.clone())).is_ok());
            assert!(tx.try_push(Owned(drops.clone())).is_ok());
            let rejected = tx.try_push(Owned(drops.clone())).err().unwrap();
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            drop(rejected);
            drop(rx.try_pop().unwrap());
            assert_eq!(drops.load(Ordering::Relaxed), 2);
        }
        drop(q);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }
    #[test]
    fn counter_wrap_keeps_fifo_and_full_detection() {
        let mut q = Queue::<usize, 4>::new();
        *q.produced.0.get_mut() = usize::MAX - 1;
        *q.consumed.0.get_mut() = usize::MAX - 1;
        let (mut tx, mut rx) = q.split();
        for cycle in 0..20 {
            for i in 0..4 {
                assert!(tx.try_push(cycle * 4 + i).is_ok());
            }
            assert_eq!(tx.try_push(1234), Err(1234));
            for i in 0..4 {
                assert_eq!(rx.try_pop(), Some(cycle * 4 + i));
            }
            assert_eq!(rx.try_pop(), None);
        }
    }
    #[test]
    fn resplit_preserves_pending_values() {
        let mut q = Queue::<u32, 2>::new();
        {
            let (mut tx, _) = q.split();
            tx.try_push(7).unwrap();
        }
        let (_, mut rx) = q.split();
        assert_eq!(rx.try_pop(), Some(7));
        assert_eq!(rx.try_pop(), None);
    }
    #[test]
    fn concurrent_handoff_preserves_sequence_and_payload() {
        let mut q = Queue::<(usize, usize), 8>::new();
        let (mut tx, mut rx) = q.split();
        thread::scope(|s| {
            s.spawn(move || {
                for n in 0..100_000 {
                    let mut v = (n, n ^ 0x12345678);
                    while let Err(back) = tx.try_push(v) {
                        v = back;
                        thread::yield_now();
                    }
                }
            });
            for n in 0..100_000 {
                let v = loop {
                    if let Some(v) = rx.try_pop() {
                        break v;
                    }
                    thread::yield_now();
                };
                assert_eq!(v, (n, n ^ 0x12345678));
            }
        });
    }
    #[test]
    fn payload_needs_send_but_not_sync() {
        let mut q = Queue::<Cell<u32>, 2>::new();
        let (mut tx, mut rx) = q.split();
        thread::scope(|s| {
            s.spawn(move || {
                assert!(tx.try_push(Cell::new(42)).is_ok());
            });
            let value = loop {
                if let Some(v) = rx.try_pop() {
                    break v;
                }
                thread::yield_now();
            };
            assert_eq!(value.into_inner(), 42);
        });
    }
}
