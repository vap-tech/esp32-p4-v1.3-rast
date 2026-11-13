//! Scoped core0 CLIC IRQs for SDMMC completion and a 1ms timeout tick (P4 rev1.x).
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::task::Waker;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(true);
static EVENTS: AtomicU32 = AtomicU32::new(0);
static SD_COUNT: AtomicU32 = AtomicU32::new(0);
static TICKS: AtomicU32 = AtomicU32::new(0);
static WAKER: atomic_waker::AtomicWaker = atomic_waker::AtomicWaker::new();
pub const SD_MASK: u32 = 0xffce; // command/data/auto-stop completion and errors, not SDIO DAT1
pub fn register(w: &Waker) {
    WAKER.register(w);
}
pub fn schedule() {
    READY.store(true, Ordering::Release);
}
pub fn notify() {
    READY.store(true, Ordering::Release);
    WAKER.wake();
}
pub fn ready() -> bool {
    READY.swap(false, Ordering::AcqRel)
}
pub fn events() -> u32 {
    EVENTS.swap(0, Ordering::AcqRel)
}
pub fn stats() -> (u32, u32) {
    (
        SD_COUNT.load(Ordering::Relaxed),
        TICKS.load(Ordering::Relaxed),
    )
}
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}
unsafe fn read(a: usize) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
unsafe fn write(a: usize, v: u32) {
    unsafe { (a as *mut u32).write_volatile(v) }
}
/// IRQ entry has saved all integer registers and CLIC mcause. No FP, timer
/// snapshots, allocations, filesystem work or controller transactions here.
pub fn dispatch(cause: u32) -> bool {
    match cause & 0xfff {
        16 => {
            unsafe {
                // Only consume events owned by the active async transaction.
                // C6 still uses polling and must retain its raw status bits.
                let flags = read(0x50083044) & read(0x50083024) & SD_MASK;
                EVENTS.fetch_or(flags, Ordering::Release);
                write(0x50083044, flags);
                if flags != 0 {
                    SD_COUNT.fetch_add(1, Ordering::Relaxed);
                }
            }
            notify();
            true
        }
        17 => {
            unsafe {
                write(0x500e206c, 1);
            }
            TICKS.fetch_add(1, Ordering::Relaxed);
            notify();
            true
        }
        _ => false,
    }
}
pub struct Guard {
    clic: [u32; 48],
    config: u32,
    threshold: u32,
    routes: [u32; 2],
    timer: [u32; 3],
    mtvec: usize,
    mstatus: usize,
}
impl Guard {
    /// # Safety
    /// Core0 only, no other interrupt owner. Own SYSTIMER target0 and SDMMC.
    /// Timer unit0 snapshots remain usable by the cooperative foreground code.
    #[cfg(target_arch = "riscv32")]
    pub unsafe fn enter() -> Self {
        assert!(!ACTIVE.swap(true, Ordering::AcqRel));
        let (mtvec, mstatus): (usize, usize);
        unsafe {
            core::arch::asm!("csrr {0}, mtvec","csrr {1}, mstatus","csrci mstatus,8",out(reg)mtvec,out(reg)mstatus,options(nostack));
            let mut clic = [0; 48];
            for (i, c) in clic.iter_mut().enumerate() {
                *c = read(0x20801000 + i * 4);
                write(0x20801000 + i * 4, *c & !(1 << 8));
            }
            let this = Self {
                clic,
                config: read(0x20800000),
                threshold: read(0x20800008),
                routes: [read(0x500d605c), read(0x500d60d4)],
                timer: [read(0x500e2000), read(0x500e2034), read(0x500e2064)],
                mtvec,
                mstatus,
            };
            READY.store(true, Ordering::Release);
            EVENTS.store(0, Ordering::Relaxed);
            SD_COUNT.store(0, Ordering::Relaxed);
            TICKS.store(0, Ordering::Relaxed);
            write(0x20800000, (this.config & !0x1e) | (3 << 1));
            write(0x20800008, 0x1f000000);
            write(0x500d605c, 16);
            write(0x500d60d4, 17);
            write(0x20801040, 0x3fc00100);
            write(0x20801044, 0x3fc00100); // level, non-vectored, priority1
            write(0x500e2000, this.timer[0] & !(1 << 24));
            write(0x500e2034, (1 << 30) | 16000);
            write(0x500e2050, 1);
            write(0x500e206c, 1);
            write(0x500e2064, this.timer[2] | 1);
            write(0x500e2000, this.timer[0] | (1 << 24));
            let vector = mtvec | 3;
            core::arch::asm!("csrw mtvec,{0}","csrsi mstatus,8",in(reg)vector,options(nostack));
            this
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(target_arch = "riscv32")]
        unsafe {
            core::arch::asm!("csrci mstatus,8", options(nostack));
            write(0x50083024, 0);
            write(0x50083000, read(0x50083000) & !(1 << 4));
            write(0x500e2000, read(0x500e2000) & !(1 << 24));
            write(0x500e2064, self.timer[2]);
            write(0x500e206c, 1);
            write(0x500e2034, self.timer[1]);
            write(0x500e2000, self.timer[0]);
            write(0x500d605c, self.routes[0]);
            write(0x500d60d4, self.routes[1]);
            write(0x20800000, self.config);
            write(0x20800008, self.threshold);
            for (i, c) in self.clic.iter().enumerate() {
                write(0x20801000 + i * 4, *c);
            }
            core::arch::asm!("csrw mtvec,{0}","csrw mstatus,{1}",in(reg)self.mtvec,in(reg)self.mstatus,options(nostack));
        }
        ACTIVE.store(false, Ordering::Release);
    }
}
