//! Single foreground future with cooperative housekeeping. No heap or RTOS.
//! Hardware IRQs and the timeout tick wake the future; idle work is never run
//! with a Rust borrow of a peripheral transaction's temporary timer handle.
use crate::hal::interrupt;
use core::{
    future::Future,
    pin::pin,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};
unsafe fn clone(_: *const ()) -> RawWaker {
    RawWaker::new(core::ptr::null(), &VTABLE)
}
unsafe fn wake(_: *const ()) {
    interrupt::schedule();
}
unsafe fn drop_waker(_: *const ()) {}
static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake, drop_waker);
pub fn run<F: Future, E>(
    future: F,
    mut idle: impl FnMut() -> Result<(), E>,
) -> Result<F::Output, E> {
    assert!(interrupt::is_active());
    let waker = unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    interrupt::schedule();
    loop {
        if interrupt::ready() {
            if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
                return Ok(result);
            }
        }
        idle()?;
    }
}
/// Sleep until a completion/timer interrupt. Caller still checks USB each tick.
pub fn sleep() {
    #[cfg(target_arch = "riscv32")]
    unsafe {
        core::arch::asm!("wfi", options(nostack));
    }
}

/// Give housekeeping a turn between transactions on the shared SD/C6 bus.
pub async fn yield_now() {
    let mut yielded = false;
    core::future::poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}
