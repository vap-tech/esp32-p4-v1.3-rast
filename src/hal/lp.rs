//! Exclusive LP core + LP TIMER target1 driver for P4 rev1.x. Never resets the
//! free-running main counter or changes target0 / the external I2C RTC.
use super::time::Timer;
use core::{arch::asm, marker::PhantomData};
const PMU: usize = 0x50115000;
const TIMER: usize = 0x50112000;
pub const CODE: usize = 0x50109000;
pub const MAIL: usize = 0x50109800;
#[derive(Debug)]
pub enum Error {
    StopTimeout,
    Clock,
    Period,
    Image,
    Busy,
    Fault { cause: u32, pc: u32, value: u32 },
}
unsafe fn read(a: usize) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
unsafe fn write(a: usize, v: u32) {
    unsafe {
        (a as *mut u32).write_volatile(v);
        asm!("fence iorw,iorw", options(nostack));
    }
}
unsafe fn modify(a: usize, mask: u32, v: u32) {
    unsafe {
        write(a, (read(a) & !mask) | v);
    }
}
pub struct Lp {
    hz: u32,
    running: bool,
    period_ms: u32,
    _exclusive: PhantomData<*mut ()>,
}
/// Stop our retained timer program even when the new image omits lp-timer.
/// # Safety
/// HP0 boot only, before any new LP owner. This is deliberately restricted
/// to our signature and boot address; an unrelated LP ROM program is untouched.
pub unsafe fn stop_owned_previous(timer: &mut Timer) -> Result<bool, Error> {
    if unsafe { read(MAIL + 60) } != crate::lp::MAGIC || unsafe { read(0x50110018) } != CODE as u32
    {
        return Ok(false);
    }
    let mut previous = Lp {
        hz: 0,
        running: true,
        period_ms: 0,
        _exclusive: PhantomData,
    };
    previous.stop(timer)?;
    Ok(true)
}
impl Lp {
    /// # Safety
    /// HP0 owns LP core/PMU controls, target1, snapshot UPDATE while LP is held,
    /// and 0x50109000..0x5010a000. No other LP program, snapshot user or retained
    /// data may use these ranges. HP deep sleep is not enabled in this driver.
    pub unsafe fn new(timer: &mut Timer) -> Result<Self, Error> {
        let mut lp = Self {
            hz: 0,
            running: false,
            period_ms: 0,
            _exclusive: PhantomData,
        };
        lp.stop(timer)?;
        let first = unsafe { lp.counter() };
        let start = timer.now_us();
        timer.delay_us(200_000);
        let last = unsafe { lp.counter() };
        let us = timer.now_us() - start;
        let hz = last.wrapping_sub(first) & ((1u64 << 48) - 1);
        lp.hz = u32::try_from((hz * 1_000_000 + us / 2) / us).map_err(|_| Error::Clock)?;
        if crate::lp::interval_ticks(lp.hz, 1000).is_none() {
            return Err(Error::Clock);
        }
        Ok(lp)
    }
    /// Caller has LP held; only this owner can touch snapshot UPDATE.
    unsafe fn counter(&mut self) -> u64 {
        unsafe {
            modify(TIMER + 0x10, 0, 1 << 28);
            let lo = read(TIMER + 0x14);
            let hi = read(TIMER + 0x18) & 0xffff;
            u64::from(lo) | (u64::from(hi) << 32)
        }
    }
    pub fn stop(&mut self, timer: &mut Timer) -> Result<(), Error> {
        unsafe {
            write(PMU + 0x18c, 0); // no wake source may re-enter while we load SRAM
            modify(TIMER + 0x0c, 1 << 31, 0);
            write(TIMER + 0x44, 1 << 31);
            write(PMU + 0x180, 1 << 31);
            modify(PMU + 0x184, 0, (1 << 29) | (1 << 30));
            write(PMU + 0x188, 1 << 31);
            modify(PMU + 0x184, 0, 1 << 18); // force stall protects against in-flight wake
        }
        let start = timer.now_us();
        while unsafe { read(PMU + 0x184) } & 3 == 0 {
            if timer.now_us() - start > 100_000 {
                return Err(Error::StopTimeout);
            }
        }
        timer.delay_us(10);
        // LP may have written its wake mask after our first disable, before
        // stall was acknowledged. Finish ownership transfer only when held.
        unsafe {
            write(PMU + 0x18c, 0);
            modify(TIMER + 0x0c, 1 << 31, 0);
            write(TIMER + 0x44, 1 << 31);
            write(PMU + 0x180, 1 << 31);
        }
        self.running = false;
        Ok(())
    }
    /// # Safety
    /// Code must be an audited position-independent LP program with entry at
    /// CODE and trap vector at CODE+0x200. It may access only the reserved LP
    /// SRAM and peripherals owned by this driver; it must tolerate forced
    /// stall at any instruction. Arbitrary bytes are not a safe Rust program.
    pub unsafe fn start(
        &mut self,
        code: &[u8],
        period_ms: u32,
        timer: &mut Timer,
    ) -> Result<(), Error> {
        let ticks = crate::lp::interval_ticks(self.hz, period_ms).ok_or(Error::Period)?;
        if code.is_empty() || code.len() > 0x700 || !code.len().is_multiple_of(4) {
            return Err(Error::Image);
        }
        self.stop(timer)?;
        unsafe {
            for (i, b) in code.as_chunks::<4>().0.iter().enumerate() {
                write(CODE + i * 4, u32::from_le_bytes(*b));
            }
            for i in 0..16 {
                write(MAIL + i * 4, 0);
            }
            write(MAIL + 32, ticks);
            write(MAIL + 60, crate::lp::MAGIC);
            write(0x50109c00, 0xa55a6996);
            write(0x50110018, CODE as u32);
            write(0x50110028, CODE as u32);
            modify(0x50120008, 0, 1 << 31);
            modify(0x50120008, 1 << 31, 0);
            modify(0x50120000, 0, 1 << 31);
            modify(0x5012000c, 1 << 31, 0);
            // One HP wake boots the code. LP itself removes HP from the mask and
            // subsequently schedules/rearms target1; HP does not maintain the timer.
            write(PMU + 0x18c, 1 << 22);
            modify(PMU + 0x184, 1 << 18, 0);
            write(PMU + 0x19c, 1 << 31);
        }
        self.running = true;
        self.period_ms = period_ms;
        Ok(())
    }
    pub fn snapshot(&self) -> Result<crate::lp::Snapshot, Error> {
        if unsafe { read(MAIL + 44) } != 0 {
            return Err(Error::Fault {
                cause: unsafe { read(MAIL + 48) },
                pc: unsafe { read(MAIL + 52) },
                value: unsafe { read(MAIL + 56) },
            });
        }
        for _ in 0..16 {
            let first = unsafe { read(MAIL) };
            unsafe {
                asm!("fence iorw,iorw", options(nostack));
            }
            let mut w = [0; 10];
            for (i, v) in w.iter_mut().enumerate() {
                *v = unsafe { read(MAIL + 4 + i * 4) };
            }
            unsafe {
                asm!("fence iorw,iorw", options(nostack));
            }
            let last = unsafe { read(MAIL) };
            if let Some(s) = crate::lp::Snapshot::decode(first, last, w) {
                return Ok(s);
            }
        }
        Err(Error::Busy)
    }
    pub fn hz(&self) -> u32 {
        self.hz
    }
    pub fn period_ms(&self) -> u32 {
        self.period_ms
    }
    pub fn running(&self) -> bool {
        self.running
    }
    pub fn diagnostic(&self) -> [u32; 6] {
        unsafe {
            [
                read(PMU + 0x184),
                read(PMU + 0x18c),
                read(0x50110190),
                read(PMU + 0x190),
                read(TIMER + 0x38),
                read(0x50111000), // slow/fast clock mux, read-only diagnostic
            ]
        }
    }
}
