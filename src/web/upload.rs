//! Lazy per-socket PSRAM staging. Slots reuse allocations until the web arena resets.
use crate::memory::{Arena, Buffer, Error, External};
pub const BLOCK: usize = match option_env!("P4_UPLOAD_BLOCK_KIB") {
    Some(v) if v.as_bytes()[0] == b'6' => 65536,
    _ => 32768,
};
pub const BLOCKS: usize = match option_env!("P4_UPLOAD_BLOCKS") {
    Some(v) if v.as_bytes()[0] == b'3' => 3,
    _ => 2,
};
pub const CHUNK: usize = BLOCK * BLOCKS;
const GUARD: usize = 64;
pub struct UploadPool<'a, 'mem, const N: usize> {
    arena: Option<&'a Arena<'mem, External>>,
    buffers: [Option<Buffer<'a, External>>; N],
    active: [bool; N],
    peak: usize,
}
impl<'a, 'mem, const N: usize> UploadPool<'a, 'mem, N> {
    pub fn new(arena: Option<&'a Arena<'mem, External>>) -> Self {
        Self {
            arena,
            buffers: [const { None }; N],
            active: [false; N],
            peak: 0,
        }
    }
    pub fn enabled(&self) -> bool {
        self.arena.is_some()
    }
    pub fn acquire(&mut self, slot: usize) -> Result<(), Error> {
        if slot >= N || self.active[slot] {
            return Err(Error::InvalidLayout);
        }
        if self.buffers[slot].is_none() {
            if let Some(idle) =
                (0..N).find(|&i| i != slot && !self.active[i] && self.buffers[i].is_some())
            {
                self.buffers[slot] = self.buffers[idle].take();
            }
        }
        if self.buffers[slot].is_none() {
            let arena = self.arena.ok_or(Error::Exhausted)?;
            let mut b = arena
                .allocate(core::alloc::Layout::from_size_align(CHUNK + GUARD * 2, 64).unwrap())?;
            let bytes = b.as_mut_slice();
            bytes[..GUARD].fill(0xa5);
            bytes[GUARD + CHUNK..].fill(0xa5);
            self.buffers[slot] = Some(b);
        }
        if !self.guards_ok(slot) {
            return Err(Error::InvalidLayout);
        }
        self.active[slot] = true;
        self.peak = self.peak.max(self.active.iter().filter(|a| **a).count());
        Ok(())
    }
    pub fn append(&mut self, slot: usize, offset: usize, bytes: &[u8]) -> Result<(), Error> {
        if slot >= N
            || !self.active[slot]
            || offset.checked_add(bytes.len()).is_none_or(|n| n > CHUNK)
        {
            return Err(Error::InvalidLayout);
        }
        let dst = &mut self.buffers[slot].as_mut().unwrap().as_mut_slice()
            [GUARD + offset..GUARD + offset + bytes.len()];
        copy_paced(dst, bytes);
        Ok(())
    }
    /// Disjoint source and free-region views: DMA holds only `source`.
    pub fn parts(
        &mut self,
        slot: usize,
        start: usize,
        length: usize,
    ) -> (&[u8], &mut [u8], &mut [u8]) {
        assert!(self.active[slot] && start + length <= CHUNK);
        let bytes = &mut self.buffers[slot].as_mut().unwrap().as_mut_slice()[GUARD..GUARD + CHUNK];
        let (before, rest) = bytes.split_at_mut(start);
        let (source, after) = rest.split_at_mut(length);
        (source, before, after)
    }
    pub fn data(&self, slot: usize, length: usize) -> &[u8] {
        assert!(self.active[slot] && length <= CHUNK);
        &self.buffers[slot].as_ref().unwrap().as_slice()[GUARD..GUARD + length]
    }
    fn guards_ok(&self, slot: usize) -> bool {
        self.buffers[slot].as_ref().is_none_or(|b| {
            let bytes = b.as_slice();
            bytes[..GUARD]
                .iter()
                .chain(bytes[GUARD + CHUNK..].iter())
                .all(|b| *b == 0xa5)
        })
    }
    pub fn release(&mut self, slot: usize) -> bool {
        let ok = self.guards_ok(slot);
        self.active[slot] = false;
        ok
    }
    pub fn stats(&self) -> (usize, usize) {
        (
            self.buffers.iter().filter(|b| b.is_some()).count(),
            self.peak,
        )
    }
}
pub fn copy_paced(dst: &mut [u8], bytes: &[u8]) {
    assert_eq!(dst.len(), bytes.len());
    // Explicit wide stores matter for uncached PSRAM; preserve arbitrary tails.
    let prefix = dst.as_ptr().align_offset(4).min(dst.len());
    dst[..prefix].copy_from_slice(&bytes[..prefix]);
    let mut at = prefix;
    while at + 4 <= bytes.len() {
        let value = if bytes[at..].as_ptr().align_offset(4) == 0 {
            // SAFETY: checked alignment, four initialized source bytes.
            unsafe { bytes.as_ptr().add(at).cast::<u32>().read() }
        } else {
            u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap())
        };
        // SAFETY: aligned prefix and four bytes within the exclusively borrowed slice.
        unsafe {
            dst.as_mut_ptr().add(at).cast::<u32>().write(value);
        }
        at += 4;
        #[cfg(target_arch = "riscv32")]
        if (at - prefix).is_multiple_of(64) {
            // Same bounded write pacing as framebuffer::write_run, so CPU
            // staging writes leave PSRAM service slots for LCD scanout.
            unsafe {
                core::arch::asm!("fence rw,rw", options(nostack));
            }
            let start = crate::hal::time::cycle_count();
            while crate::hal::time::cycle_count().wrapping_sub(start) < 320 {
                core::hint::spin_loop();
            }
        }
    }
    dst[at..].copy_from_slice(&bytes[at..]);
    #[cfg(target_arch = "riscv32")]
    unsafe {
        core::arch::asm!("fence rw,rw", options(nostack));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    #[repr(align(64))]
    struct Bytes([u8; (CHUNK + GUARD * 2) * 2]);
    #[test]
    fn ring_wrap_preserves_inflight_source_and_byte_order() {
        let mut bytes = std::boxed::Box::new(Bytes([0; (CHUNK + GUARD * 2) * 2]));
        let arena = unsafe {
            Arena::from_raw(
                core::ptr::NonNull::new(bytes.0.as_mut_ptr()).unwrap(),
                bytes.0.len(),
            )
        };
        let mut pool = UploadPool::<2>::new(Some(&arena));
        pool.acquire(0).unwrap();
        let total = CHUNK * 5 + 513;
        let mut produced = 0;
        let mut consumed = 0;
        let mut head = 0;
        let mut queued = 0;
        while consumed < total {
            if queued == 0 {
                let n = BLOCK.min(total - produced);
                let data: std::vec::Vec<_> =
                    (produced..produced + n).map(|i| (i % 251) as u8).collect();
                pool.append(0, head, &data).unwrap();
                queued += n;
                produced += n;
            }
            let length = queued.min(32768).min(CHUNK - head);
            let (source, before, after) = pool.parts(0, head, length);
            // Simulate a pending DMA read while the producer fills ALL free space.
            while queued < CHUNK && produced < total {
                let tail = (head + queued) % CHUNK;
                let n = (CHUNK - queued)
                    .min(CHUNK - tail)
                    .min(total - produced)
                    .min(1003);
                let data: std::vec::Vec<_> =
                    (produced..produced + n).map(|i| (i % 251) as u8).collect();
                let dst = if tail < head {
                    &mut before[tail..tail + n]
                } else {
                    &mut after[tail - head - length..tail - head - length + n]
                };
                copy_paced(dst, &data);
                queued += n;
                produced += n;
            }
            for (i, value) in source.iter().enumerate() {
                assert_eq!(*value, ((consumed + i) % 251) as u8);
            }
            consumed += length;
            queued -= length;
            head = (head + length) % CHUNK;
        }
        assert_eq!(produced, total);
        assert_eq!(queued, 0);
        assert!(pool.release(0));
    }
    #[test]
    fn lazy_slots_reuse_buffers_and_preserve_unaligned_tails() {
        let mut bytes = std::boxed::Box::new(Bytes([0; (CHUNK + GUARD * 2) * 2]));
        let mut arena = unsafe {
            Arena::from_raw(
                core::ptr::NonNull::new(bytes.0.as_mut_ptr()).unwrap(),
                bytes.0.len(),
            )
        };
        {
            let mut pool = UploadPool::<2>::new(Some(&arena));
            assert_eq!(arena.stats().used, 0);
            pool.acquire(0).unwrap();
            assert!(pool.acquire(0).is_err());
            pool.append(0, 0, &[9; 1003]).unwrap();
            pool.append(0, 1003, &[7; 2049]).unwrap();
            assert_eq!(&pool.data(0, 3052)[..1003], &[9; 1003]);
            assert_eq!(&pool.data(0, 3052)[1003..], &[7; 2049]);
            assert!(pool.append(0, CHUNK - 1, &[0; 2]).is_err());
            let first_used = arena.stats().used;
            assert!(pool.release(0));
            pool.acquire(1).unwrap();
            assert_eq!(arena.stats().used, first_used);
            assert_eq!(pool.stats(), (1, 1));
            assert!(pool.release(1));
            pool.acquire(0).unwrap();
            pool.acquire(1).unwrap();
            pool.append(1, 0, &[3; CHUNK]).unwrap();
            let used = arena.stats().used;
            assert!(pool.release(0));
            pool.acquire(0).unwrap();
            assert_eq!(arena.stats().used, used);
            assert_eq!(pool.stats(), (2, 2));
            assert!(pool.release(0));
            assert!(pool.release(1));
        }
        arena.reset();
        assert_eq!(arena.stats().used, 0);
    }
}
