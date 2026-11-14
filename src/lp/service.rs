use crate::hal::{lp::Lp, time::Timer, usb_serial::Console};
use core::{
    arch::{asm, global_asm},
    cell::UnsafeCell,
    fmt::Write,
    sync::atomic::{AtomicBool, Ordering},
};
global_asm!(include_str!("timer.S"));
struct Local(UnsafeCell<Option<Lp>>);
// Only HP0 foreground accesses the driver. No LP/HP1/ISR Rust references.
unsafe impl Sync for Local {}
static DRIVER: Local = Local(UnsafeCell::new(None));
static BUSY: AtomicBool = AtomicBool::new(false);
struct Borrow;
impl Borrow {
    fn take() -> Option<Self> {
        let hart: usize;
        unsafe {
            asm!("csrr {0},mhartid",out(reg)hart,options(nostack));
        }
        assert_eq!(hart, 0);
        BUSY.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self)
    }
    fn slot(&mut self) -> &mut Option<Lp> {
        unsafe { &mut *DRIVER.0.get() }
    }
}
impl Drop for Borrow {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}
fn image() -> &'static [u8] {
    extern "C" {
        static lp_timer_start: u8;
        static lp_timer_end: u8;
    }
    let first = core::ptr::addr_of!(lp_timer_start);
    let end = core::ptr::addr_of!(lp_timer_end);
    unsafe { core::slice::from_raw_parts(first, end as usize - first as usize) }
}
/// # Safety
/// Once on HP0, owns LP program/LP SRAM and LP TIMER1. No HP deep sleep or
/// other RTC timer snapshot owner. Driver stops old LP code before loading.
pub unsafe fn init(timer: &mut Timer, console: &mut Console) -> Result<(), crate::hal::lp::Error> {
    let mut borrow = Borrow::take().unwrap();
    assert!(borrow.slot().is_none());
    let mut driver = unsafe { Lp::new(timer) }?;
    unsafe {
        driver.start(image(), 1000, timer)?;
    }
    let start = timer.now_us();
    while driver.snapshot().is_err() && timer.now_us() - start < 500_000 {
        core::hint::spin_loop();
    }
    let result = driver.snapshot();
    report(&driver, console);
    *borrow.slot() = Some(driver);
    result.map(|_| ())
}
fn report(lp: &Lp, console: &mut Console) {
    let _ = writeln!(
        console,
        "LP TIMER running={} period_ms={} slow_hz={} bytes={} snapshot={:?} hw={:08x?}",
        lp.running(),
        lp.period_ms(),
        lp.hz(),
        image().len(),
        lp.snapshot(),
        lp.diagnostic()
    );
}
pub fn command(text: &str, timer: &mut Timer, console: &mut Console) -> bool {
    let Some(args) = text.strip_prefix("system lp") else {
        return false;
    };
    if !args.is_empty() && !args.starts_with(' ') {
        return false;
    }
    let Some(mut borrow) = Borrow::take() else {
        return true;
    };
    let Some(lp) = borrow.slot().as_mut() else {
        let _ = writeln!(console, "LP TIMER unavailable");
        return true;
    };
    match args.trim() {
        "" | "status" => {}
        "stop" => {
            let _ = writeln!(console, "LP TIMER STOP {:?}", lp.stop(timer));
        }
        "start" => {
            let _ = writeln!(console, "LP TIMER START {:?}", unsafe {
                lp.start(image(), 1000, timer)
            });
        }
        x if x.starts_with("start ") => {
            let r = x[6..]
                .parse::<u32>()
                .map_err(|_| crate::hal::lp::Error::Period)
                .and_then(|ms| unsafe { lp.start(image(), ms, timer) });
            let _ = writeln!(console, "LP TIMER START {:?}", r);
        }
        _ => {
            let _ = writeln!(
                console,
                "LP TIMER commands: status | start [100..60000 ms] | stop"
            );
        }
    }
    report(lp, console);
    true
}
