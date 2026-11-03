//! 52-bit SYSTIMER unit 0, XTAL 40 MHz / 2.5 = 16 MHz.
use core::marker::PhantomData;

const BASE: usize = 0x500e_2000;
const CLOCK: usize = 0x500e_6000;
const US_MASK: u64 = (1 << 48) - 1;

pub struct Timer {
    _exclusive: PhantomData<*mut ()>,
}

impl Timer {
    /// Start a fresh epoch for SYSTIMER unit 0.
    ///
    /// # Safety
    /// Caller must own SYSTIMER and its clock/reset controls exclusively. Board
    /// crystal must be 40 MHz. No other core/ISR may access snapshot or load regs.
    pub unsafe fn new() -> Self {
        unsafe {
            let bus = (CLOCK + 0x1c) as *mut u32;
            bus.write_volatile(bus.read_volatile() | (1 << 23));
            let clock = (CLOCK + 0x98) as *mut u32;
            clock.write_volatile((clock.read_volatile() & !(1 << 29)) | (1 << 30));
            let reset = (CLOCK + 0xc4) as *mut u32;
            reset.write_volatile(reset.read_volatile() | (1 << 5));
            reset.write_volatile(reset.read_volatile() & !(1 << 5));
            // Register clock enabled; unit 0 disabled while loading zero.
            (BASE as *mut u32).write_volatile(1 << 31);
            ((BASE + 0x0c) as *mut u32).write_volatile(0);
            ((BASE + 0x10) as *mut u32).write_volatile(0);
            ((BASE + 0x5c) as *mut u32).write_volatile(1);
            // Unit 0 runs even when the CPU stalls; alarms and unit 1 unused.
            (BASE as *mut u32).write_volatile((1 << 31) | (1 << 30));
        }
        Self {
            _exclusive: PhantomData,
        }
    }

    /// Microseconds since initialization, wrapping after 2^48 us (~8.9 years).
    pub fn now_us(&mut self) -> u64 {
        unsafe {
            let op = (BASE + 4) as *mut u32;
            // VALUE_VALID is W1C: discard a previous snapshot before UPDATE.
            op.write_volatile((1 << 29) | (1 << 30));
            while op.read_volatile() & (1 << 29) == 0 {
                core::hint::spin_loop();
            }
            let high = ((BASE + 0x40) as *const u32).read_volatile() & 0x000f_ffff;
            let low = ((BASE + 0x44) as *const u32).read_volatile();
            (((high as u64) << 32) | low as u64) / 16
        }
    }

    /// Busy wait for at least `us` microseconds; does not sleep or disable IRQs.
    pub fn delay_us(&mut self, us: u32) {
        let start = self.now_us();
        while (self.now_us().wrapping_sub(start) & US_MASK) < u64::from(us) {
            core::hint::spin_loop();
        }
    }
}
