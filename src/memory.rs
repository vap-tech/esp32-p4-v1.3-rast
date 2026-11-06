//! Explicit region arenas, no global heap. Allocations live until reset, not Drop.
//! Internal and PSRAM buffers have distinct types; only internal memory can become
//! an AHB DMA buffer. One arena can issue disjoint buffers, reset needs &mut self.
use core::{alloc::Layout, cell::Cell, marker::PhantomData, ptr::NonNull};

pub enum Internal {}
pub enum External {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Exhausted,
    InvalidLayout,
    NotAhbMemory,
}
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub capacity: usize,
    pub used: usize,
    pub high_water: usize,
}

pub struct Arena<'a, Region> {
    base: NonNull<u8>,
    length: usize,
    cursor: Cell<usize>,
    peak: Cell<usize>,
    _owner: PhantomData<(&'a mut [u8], Region, *mut ())>,
}
impl<'a, R> Arena<'a, R> {
    /// # Safety
    /// Own this entire writable CPU-addressable range for 'a, no aliases or DMA.
    /// It must remain mapped/powered. Region must describe its actual hardware.
    /// PSRAM must be initialized; use only one cache alias for its allocations.
    pub unsafe fn from_raw(base: NonNull<u8>, length: usize) -> Self {
        assert!(
            length <= isize::MAX as usize && (base.as_ptr() as usize).checked_add(length).is_some()
        );
        Self {
            base,
            length,
            cursor: Cell::new(0),
            peak: Cell::new(0),
            _owner: PhantomData,
        }
    }
    pub fn stats(&self) -> Stats {
        Stats {
            capacity: self.length,
            used: self.cursor.get(),
            high_water: self.peak.get(),
        }
    }
    /// Zero-initialized allocation. Region space is reclaimed only by reset().
    pub fn allocate(&self, layout: Layout) -> Result<Buffer<'_, R>, Error> {
        if layout.size() == 0 {
            return Err(Error::InvalidLayout);
        }
        let address = (self.base.as_ptr() as usize)
            .checked_add(self.cursor.get())
            .ok_or(Error::Exhausted)?;
        let aligned = address
            .checked_add(layout.align() - 1)
            .ok_or(Error::Exhausted)?
            & !(layout.align() - 1);
        let offset = aligned - (self.base.as_ptr() as usize);
        let end = offset
            .checked_add(layout.size())
            .filter(|&end| end <= self.length)
            .ok_or(Error::Exhausted)?;
        let ptr = unsafe { NonNull::new_unchecked(self.base.as_ptr().add(offset)) };
        unsafe {
            ptr.as_ptr().write_bytes(0, layout.size());
        }
        self.cursor.set(end);
        self.peak.set(self.peak.get().max(end));
        Ok(Buffer {
            ptr,
            len: layout.size(),
            _owner: PhantomData,
        })
    }
    /// Borrow checking prevents reset while any allocation is still in use.
    pub fn reset(&mut self) {
        self.cursor.set(0);
    }
}
pub struct Buffer<'a, Region> {
    ptr: NonNull<u8>,
    len: usize,
    _owner: PhantomData<(&'a mut [u8], Region, *mut ())>,
}
impl<R> Buffer<'_, R> {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}
pub enum CpuOwned {}
pub enum DmaOwned {}
/// A cache-line-isolated internal buffer, with explicit cache ownership handoff.
/// This token does not program or own a DMA channel. A HAL starting hardware must
/// keep it alive until the channel is stopped; dropping it does not stop DMA.
#[must_use]
pub struct AhbDmaBuffer<'a, State> {
    buffer: Buffer<'a, Internal>,
    _state: PhantomData<State>,
}
impl<'a> Buffer<'a, Internal> {
    pub fn into_ahb_dma(self) -> Result<AhbDmaBuffer<'a, CpuOwned>, (Self, Error)> {
        let address = self.ptr.as_ptr() as usize;
        if !crate::hal::memory::valid_ahb_range(address, self.len) {
            return Err((self, Error::NotAhbMemory));
        }
        Ok(AhbDmaBuffer {
            buffer: self,
            _state: PhantomData,
        })
    }
}
impl<'a> AhbDmaBuffer<'a, CpuOwned> {
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.buffer.as_mut_slice()
    }
    pub fn publish(
        self,
        timer: &mut crate::hal::time::Timer,
    ) -> Result<AhbDmaBuffer<'a, DmaOwned>, (Self, crate::hal::memory::Error)> {
        if let Err(error) = crate::hal::memory::sync_internal(
            self.buffer.ptr.as_ptr() as usize,
            self.buffer.len,
            false,
            timer,
        ) {
            return Err((self, error));
        }
        Ok(AhbDmaBuffer {
            buffer: self.buffer,
            _state: PhantomData,
        })
    }
}
impl<'a> AhbDmaBuffer<'a, DmaOwned> {
    pub fn address(&self) -> u32 {
        self.buffer.ptr.as_ptr() as u32
    }
    pub fn len(&self) -> usize {
        self.buffer.len
    }
    pub fn is_empty(&self) -> bool {
        self.buffer.len == 0
    }
    /// # Safety
    /// Hardware must have released this entire buffer and no DMA may access it
    /// until the next publish. Applies to RX as well as TX.
    pub unsafe fn reclaim(
        self,
        timer: &mut crate::hal::time::Timer,
    ) -> Result<AhbDmaBuffer<'a, CpuOwned>, (Self, crate::hal::memory::Error)> {
        if let Err(error) = crate::hal::memory::sync_internal(
            self.buffer.ptr.as_ptr() as usize,
            self.buffer.len,
            true,
            timer,
        ) {
            return Err((self, error));
        }
        Ok(AhbDmaBuffer {
            buffer: self.buffer,
            _state: PhantomData,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocations_are_disjoint_aligned_zeroed_and_bounded() {
        let mut backing = [0xa5u8; 512];
        let mut arena = unsafe {
            Arena::<Internal>::from_raw(NonNull::new(backing.as_mut_ptr()).unwrap(), backing.len())
        };
        {
            let mut a = arena
                .allocate(Layout::from_size_align(64, 64).unwrap())
                .unwrap();
            let b = arena
                .allocate(Layout::from_size_align(128, 128).unwrap())
                .unwrap();
            assert_eq!(a.ptr.as_ptr() as usize % 64, 0);
            assert_eq!(b.ptr.as_ptr() as usize % 128, 0);
            assert!(a.ptr.as_ptr() as usize + 64 <= b.ptr.as_ptr() as usize);
            a.as_mut_slice().fill(42);
            assert!(b.as_slice().iter().all(|&v| v == 0));
            assert!(matches!(
                arena.allocate(Layout::from_size_align(513, 1).unwrap()),
                Err(Error::Exhausted)
            ));
            assert!(matches!(
                arena.allocate(Layout::from_size_align(0, 1).unwrap()),
                Err(Error::InvalidLayout)
            ));
        }
        let peak = arena.stats().high_water;
        arena.reset();
        assert_eq!(arena.stats().used, 0);
        assert_eq!(arena.stats().high_water, peak);
        let a = arena
            .allocate(Layout::from_size_align(64, 64).unwrap())
            .unwrap();
        assert!(a.as_slice().iter().all(|&v| v == 0));
    }
}
