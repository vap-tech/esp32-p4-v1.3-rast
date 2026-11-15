//! Shared exclusive ownership of the entire 2D-DMA block (including reset).
use core::sync::atomic::{AtomicBool, Ordering};
static OWNED: AtomicBool = AtomicBool::new(false);
pub(crate) struct Claim;
impl Claim {
    pub(crate) fn acquire() -> Option<Self> {
        OWNED
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self)
    }
}
impl Drop for Claim {
    fn drop(&mut self) {
        OWNED.store(false, Ordering::Release);
    }
}
pub(crate) fn bus_address(ptr: usize, len: usize) -> Option<u32> {
    if ptr & 63 != 0 {
        return None;
    }
    let end = ptr.checked_add(len)?;
    if ptr >= 0x4ff00000 && end <= 0x4ffc0000 {
        Some(ptr as u32)
    } else if ptr >= 0x88400000 && end <= 0x8a000000 {
        Some((ptr - 0x40000000) as u32)
    } else {
        None
    }
}
