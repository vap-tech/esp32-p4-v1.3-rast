//! Isolated boot-time HP1 probe. No shared drivers, allocator or interrupt state.
use core::{
    arch::{asm, global_asm},
    fmt::Write,
    sync::atomic::{AtomicU32, Ordering},
};
use esp32_p4::hal::{time::Timer, usb_serial::Console};

#[repr(C, align(64))]
struct Mailbox([AtomicU32; 16]);
#[no_mangle]
static HP1_MAILBOX: Mailbox = Mailbox([const { AtomicU32::new(0) }; 16]);
#[repr(C, align(16))]
struct Stack([u32; 2048]);
#[no_mangle]
static mut HP1_STACK: Stack = Stack([0; 2048]);

// All communication uses the uncached SRAM alias on BOTH cores. HP1 never
// calls Rust/ROM or touches peripheral drivers. It has its own trap vector.
global_asm!(
    r#"
.section .text.hp1_probe,"ax"
.option push
.option norelax
.balign 4
.global hp1_probe_entry
hp1_probe_entry:
    csrci mstatus, 8
    csrw mie, zero
    la sp, HP1_STACK
    li t0, 0x40002000
    add sp, sp, t0
    la t0, hp1_probe_trap
    csrw mtvec, t0
    la t0, HP1_MAILBOX
    li t1, 0x40000000
    add t0, t0, t1
    sw sp, 28(t0)
    csrr t1, mhartid
    sw t1, 4(t0)
    csrr t1, misa
    sw t1, 8(t0)
    fence rw, rw
    li t1, 1
    sw t1, 0(t0)
1:
    lw t1, 12(t0)
    beqz t1, 1b
    fence rw, rw
    li t2, -1
    beq t1, t2, 3f
    li t2, 0x9e3779b9
    xor t2, t2, t1
    li t3, 256
2:
    slli t4, t2, 13
    xor t2, t2, t4
    srli t4, t2, 17
    xor t2, t2, t4
    slli t4, t2, 5
    xor t2, t2, t4
    addi t3, t3, -1
    bnez t3, 2b
    sw t2, 16(t0)
    sw zero, 12(t0)
    fence rw, rw
    sw t1, 20(t0)
    j 1b
3:
    fence rw, rw
    li t1, 2
    sw t1, 0(t0)
4:
    wfi
    j 4b
.balign 4
hp1_probe_trap:
    la t0, HP1_MAILBOX
    li t1, 0x40000000
    add t0, t0, t1
    csrr t1, mcause
    sw t1, 24(t0)
    csrr t1, mepc
    sw t1, 32(t0)
    csrr t1, mtval
    sw t1, 36(t0)
    fence rw, rw
    li t1, 3
    sw t1, 0(t0)
5:
    wfi
    j 5b
.option pop
"#
);

fn mailbox() -> &'static Mailbox {
    unsafe { &*((core::ptr::addr_of!(HP1_MAILBOX) as usize + 0x40000000) as *const Mailbox) }
}
unsafe fn modify(address: usize, mask: u32, value: u32) {
    let p = address as *mut u32;
    unsafe {
        p.write_volatile((p.read_volatile() & !mask) | value);
        asm!("fence iorw, iorw", options(nostack));
    }
}
fn wait(timer: &mut Timer, index: usize, value: u32) -> bool {
    let start = timer.now_us();
    while mailbox().0[index].load(Ordering::Acquire) != value {
        if mailbox().0[0].load(Ordering::Acquire) == 3
            || timer.now_us().wrapping_sub(start) > 500_000
        {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}
fn reference(request: u32) -> u32 {
    let mut x = request ^ 0x9e3779b9;
    for _ in 0..256 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
    }
    x
}

/// # Safety
/// Once, on HP0 before enabling caches/IRQs or giving any other owner HP1.
/// HP1 must be at its ROM waiting entry, not executing an application.
pub unsafe fn run(timer: &mut Timer, console: &mut Console) {
    // Laboratory image only: give the USB monitor time to attach after reset.
    timer.delay_us(3_000_000);
    extern "C" {
        fn hp1_probe_entry();
    }
    let m = mailbox();
    let stack = (core::ptr::addr_of_mut!(HP1_STACK) as usize + 0x40000000) as *mut u32;
    for i in 0..2048 {
        unsafe { stack.add(i).write_volatile(0xa55a6996) };
    }
    let _ = writeln!(
        console,
        "HP1 PROBE starting entry={:08x}",
        hp1_probe_entry as *const () as usize
    );
    unsafe {
        modify(0x50115200, 0xff << 16, 0xff << 16); // PMU unstall HP1
        modify(0x500e6014, 0, 1 << 4); // HP1 clock
        modify(0x500e60c0, 1 << 8, 0); // release HP1 global reset
        let boot: unsafe extern "C" fn(u32) = core::mem::transmute(0x4fc000a8usize);
        boot(hp1_probe_entry as *const () as u32);
    }
    let mut passed = 0;
    let start = timer.now_us();
    let ready = wait(timer, 0, 1);
    // Match IDF: once HP1 has left ROM, remove the persistent ROM entry hook.
    // Also clear it on timeout so a subsequent reset cannot jump into this probe.
    unsafe {
        let boot: unsafe extern "C" fn(u32) = core::mem::transmute(0x4fc000a8usize);
        boot(0);
    }
    if ready && m.0[1].load(Ordering::Relaxed) == 1 {
        for request in 1..=1000 {
            m.0[3].store(request, Ordering::Release);
            if !wait(timer, 5, request) || m.0[4].load(Ordering::Acquire) != reference(request) {
                break;
            }
            passed += 1;
        }
    }
    m.0[3].store(u32::MAX, Ordering::Release);
    let parked = wait(timer, 0, 2);
    // No stack/mailbox reclamation, even on timeout. Do not infer a successful
    // hardware stall from CORESTALLED when HP1 may already be in WFI (IDF caveat).
    unsafe {
        modify(0x50115200, 0xff << 16, 0x86 << 16);
    }
    let guard = unsafe { stack.read_volatile() == 0xa55a6996 };
    let _ = writeln!(console, "HP1 PROBE ready={} hart={} misa={:08x} sp={:08x} jobs={}/1000 us={} parked={} guard={} trap={:08x}/{:08x}/{:08x}", ready, m.0[1].load(Ordering::Relaxed),m.0[2].load(Ordering::Relaxed),m.0[7].load(Ordering::Relaxed),passed,timer.now_us().wrapping_sub(start),parked,guard,m.0[6].load(Ordering::Relaxed),m.0[8].load(Ordering::Relaxed),m.0[9].load(Ordering::Relaxed));
}
