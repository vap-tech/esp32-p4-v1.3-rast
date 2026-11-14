//! HP1 startup and FROM_CPU software interrupt sources (P4 rev1.x).
//! No scheduler, shared allocator, peripheral transfer or cache maintenance.
#[cfg(target_arch = "riscv32")]
unsafe fn modify(a: usize, mask: u32, value: u32) {
    unsafe {
        let p = a as *mut u32;
        p.write_volatile((p.read_volatile() & !mask) | value);
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
}
/// # Safety
/// HP0 only, once per boot; HP1 is in its ROM waiting entry, not in an
/// application. Publish all entry data first, reserve its stack/code until
/// reset, and own HP1 clock/reset/PMU controls. Does not wait for readiness.
#[cfg(target_arch = "riscv32")]
pub unsafe fn start_hp1(entry: unsafe extern "C" fn() -> !) {
    unsafe {
        core::arch::asm!("fence rw,rw", "fence.i", options(nostack));
        modify(0x50115200, 0xff << 16, 0xff << 16);
        modify(0x500e6014, 0, 1 << 4);
        modify(0x500e60c0, 1 << 8, 0);
        set_rom_entry(entry as *const () as usize as u32);
    }
}
/// Clear the persistent ROM entry after HP1 has acknowledged entering our code.
/// # Safety
/// HP0 owns HP1 startup. No concurrent ROM boot-address operation.
#[cfg(target_arch = "riscv32")]
pub unsafe fn clear_rom_entry() {
    unsafe {
        set_rom_entry(0);
    }
}
#[cfg(target_arch = "riscv32")]
unsafe fn set_rom_entry(address: u32) {
    unsafe {
        let f: unsafe extern "C" fn(u32) = core::mem::transmute(0x4fc000a8usize);
        f(address);
    }
}
/// # Safety
/// HP0 owns HP1. First obtain a software quiescence acknowledgment; retain
/// HP1 memory even on timeout. CORESTALLED is unreliable when HP1 is in WFI.
#[cfg(target_arch = "riscv32")]
pub unsafe fn stall_hp1() {
    unsafe {
        modify(0x50115200, 0xff << 16, 0x86 << 16);
    }
}

/// Sole writer of a level-sensitive FROM_CPU source. Notifications coalesce.
/// Send only AFTER publishing queue data. This is not a cache writeback.
pub struct Sender {
    line: u8,
}
/// Sole acknowledger of the source, usually held by the target's ISR.
pub struct Receiver {
    line: u8,
}
/// # Safety
/// Own the source for the lifetime of both endpoints: no other CPU/ISR/ROM
/// client may write it, and route it only to the intended target. Configure
/// CLIC separately. Disable/unroute before releasing this reservation.
/// Lines 0/1 are reserved for HP0->HP1 / HP1->HP0 in our lab protocol.
#[cfg(target_arch = "riscv32")]
pub unsafe fn claim(line: u8) -> (Sender, Receiver) {
    assert!(line < 4);
    (Sender { line }, Receiver { line })
}
#[cfg(target_arch = "riscv32")]
fn signal(line: u8, value: u32) {
    unsafe {
        core::arch::asm!("fence iorw, iorw", options(nostack));
        ((0x500e5010 + usize::from(line) * 4) as *mut u32).write_volatile(value);
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
}
impl Sender {
    #[cfg(target_arch = "riscv32")]
    pub fn notify(&mut self) {
        signal(self.line, 1);
    }
    pub fn line(&self) -> u8 {
        self.line
    }
}
impl Receiver {
    /// Clear before draining/rechecking the queue, never clear after deciding
    /// to sleep. A concurrent new notification must remain pending.
    #[cfg(target_arch = "riscv32")]
    pub fn acknowledge(&mut self) {
        signal(self.line, 0);
    }
    pub fn line(&self) -> u8 {
        self.line
    }
    /// Address in the selected core's matrix. No register is changed here.
    pub fn route_address(&self, core: u8) -> Option<usize> {
        (core < 2)
            .then_some(0x500d6000 + usize::from(core) * 0x800 + 0x13c + usize::from(self.line) * 4)
    }
}

/// Avoid the empty-queue / WFI lost-wakeup race on the CURRENT HP core.
/// # Safety
/// A locally enabled level IRQ must become pending after each queue
/// publication. CLIC, threshold and vector must be configured. No nested IRQ
/// owner or outstanding Rust borrow of ISR-owned state. `empty` must recheck
/// the queue predicate, and must neither enable IRQs nor clear notifications.
/// Do not use this to wait for DMA/peripherals without their wakeup contract.
#[cfg(target_arch = "riscv32")]
pub unsafe fn wait_if(empty: impl FnOnce() -> bool) {
    let prior: usize;
    unsafe {
        core::arch::asm!("csrrci {0},mstatus,8", out(reg)prior, options(nostack));
    }
    // WFI can wake on a locally enabled pending interrupt even with global
    // MIE=0. The pending level cannot be acknowledged by an ISR in this gap.
    if empty() {
        unsafe {
            core::arch::asm!("wfi", options(nostack));
        }
    }
    if prior & 8 != 0 {
        unsafe {
            core::arch::asm!("csrsi mstatus,8", options(nostack));
        }
    }
}
