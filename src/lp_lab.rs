//! LP startup without IDF runtime. Integer-only position-independent blob.
use core::{
    arch::{asm, global_asm},
    fmt::Write,
};
use esp32_p4::hal::{time::Timer, usb_serial::Console};
const CODE: usize = 0x50109000;
const MAIL: usize = 0x50109800;
// Leave low LP RAM and the ROM's high LP RAM reservation untouched.
global_asm!(
    r#"
.section .rodata.lp_probe,"a"
.option push
.option norelax
.option norvc
.balign 512
.global lp_probe_start
lp_probe_start:
    csrci mstatus, 8
    csrw mie, zero
    li sp, 0x5010a000
    li t0, 0x50109200
    csrw mtvec, t0
    li t0, 0x50115180
    li t1, 0x80000000
    sw t1, 0(t0)
    fence iorw, iorw
    li t0, 0x50109800
    lw t1, 0(t0)
    addi t1, t1, 1
    sw t1, 0(t0)
    csrr t1, mhartid
    sw t1, 4(t0)
    csrr t1, misa
    sw t1, 8(t0)
    sw sp, 28(t0)
1:
    lw t1, 12(t0)
    beqz t1, 1b
    fence rw, rw
    li t2, -1
    beq t1, t2, 2f
    li t2, 0x6c705034
    xor t2, t2, t1
    sw t2, 16(t0)
    sw zero, 12(t0)
    fence rw, rw
    sw t1, 20(t0)
    j 1b
2:
    sw zero, 12(t0)
    fence rw, rw
    sw t1, 20(t0)
    li t0, 0x50115188
    li t1, 0x80000000
    sw t1, 0(t0)
    fence iorw, iorw
3:
    wfi
    j 3b
.balign 512
    li t0, 0x50109800
    csrr t1, mcause
    sw t1, 32(t0)
    csrr t1, mepc
    sw t1, 36(t0)
    csrr t1, mtval
    sw t1, 40(t0)
    li t1, -1
    sw t1, 24(t0)
4:
    wfi
    j 4b
.balign 4
.global lp_probe_end
lp_probe_end:
.option pop
"#
);
unsafe fn read(a: usize) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
unsafe fn write(a: usize, v: u32) {
    unsafe {
        (a as *mut u32).write_volatile(v);
        asm!("fence iorw, iorw", options(nostack));
    }
}
unsafe fn modify(a: usize, mask: u32, value: u32) {
    unsafe { write(a, (read(a) & !mask) | value) };
}
fn wait(timer: &mut Timer, offset: usize, value: u32) -> bool {
    let start = timer.now_us();
    while unsafe { read(MAIL + offset) } != value {
        if timer.now_us().wrapping_sub(start) > 500_000 {
            return false;
        }
    }
    true
}
/// # Safety
/// Boot-time exclusive ownership of LP core, its PMU controls and
/// 0x50109000..0x5010a000; no HP sleep, retained user data or other LP program.
pub unsafe fn run(timer: &mut Timer, console: &mut Console) {
    extern "C" {
        static lp_probe_start: u8;
        static lp_probe_end: u8;
    }
    timer.delay_us(3_000_000);
    let _ = writeln!(
        console,
        "LP BEFORE boot={:08x} pwr0={:08x} clk={:08x} reset={:08x}",
        unsafe { read(0x50110018) },
        unsafe { read(0x50115184) },
        unsafe { read(0x50120000) },
        unsafe { read(0x50120008) }
    );
    let length =
        core::ptr::addr_of!(lp_probe_end) as usize - core::ptr::addr_of!(lp_probe_start) as usize;
    assert!(length <= 0x700 && length.is_multiple_of(4));
    unsafe {
        // Stop before loading, matching IDF's LP stop/load/run ordering.
        write(0x5011518c, 0);
        write(0x50115180, 1 << 31); // acknowledge retained HP wake request
        modify(0x50115184, 0, (1 << 29) | (1 << 30));
        write(0x50115188, 1 << 31);
    }
    timer.delay_us(1000);
    unsafe {
        for i in 0..length / 4 {
            write(
                CODE + i * 4,
                read(core::ptr::addr_of!(lp_probe_start) as usize + i * 4),
            );
        }
        for i in 0..16 {
            write(MAIL + i * 4, 0);
        }
        write(0x50109c00, 0xa55a6996); // bottom of separate 1 KiB LP stack
        write(0x50110018, CODE as u32); // direct boot; deliberately skip LP ROM
        write(0x50110028, CODE as u32);
        // LPPERI LP-core reset, then enable its bus clock.
        modify(0x50120008, 0, 1 << 31);
        modify(0x50120008, 1 << 31, 0);
        modify(0x50120000, 0, 1 << 31);
        modify(0x5012000c, 1 << 31, 0);
        write(0x5011518c, 1 << 22); // HP trigger is the sole wake source
        modify(0x50115184, 1 << 18, 0); // release any boot-ROM forced stall
        write(0x5011519c, 1 << 31);
    }
    let mut boots = 0;
    let mut jobs = 0;
    for boot in 1..=3 {
        if !wait(timer, 0, boot) {
            break;
        }
        boots = boot;
        for n in 1..=100 {
            let req = boot * 1000 + n;
            unsafe {
                write(MAIL + 12, req);
            }
            if !wait(timer, 20, req) || unsafe { read(MAIL + 16) } != req ^ 0x6c705034 {
                break;
            }
            jobs += 1;
        }
        unsafe {
            write(MAIL + 12, u32::MAX);
        }
        if !wait(timer, 20, u32::MAX) {
            break;
        }
        timer.delay_us(10_000);
        if boot < 3 {
            unsafe {
                write(0x5011519c, 1 << 31);
            }
        }
    }
    unsafe {
        write(0x5011518c, 0);
        write(0x50115188, 1 << 31);
    }
    let _ = writeln!(
        console,
        "LP DEBUG pc={:08x} exc={:08x} wake={:08x} reject={:08x} state={:08x} intr={:08x}",
        unsafe { read(0x50110190) },
        unsafe { read(0x50110194) },
        unsafe { read(0x50115190) },
        unsafe { read(0x50115198) },
        unsafe { read(0x501151a4) },
        unsafe { read(0x50115174) }
    );
    let _ = writeln!(
        console,
        "LP AFTER trap={:08x}/{:08x}/{:08x}/{:08x} pwr0={:08x} boot={:08x} code={:08x}",
        unsafe { read(MAIL + 24) },
        unsafe { read(MAIL + 32) },
        unsafe { read(MAIL + 36) },
        unsafe { read(MAIL + 40) },
        unsafe { read(0x50115184) },
        unsafe { read(0x50110018) },
        unsafe { read(CODE) }
    );
    let _ = writeln!(
        console,
        "LP PROBE bytes={} boots={}/3 jobs={}/300 hart={} misa={:08x} sp={:08x} guard={}",
        length,
        boots,
        jobs,
        unsafe { read(MAIL + 4) },
        unsafe { read(MAIL + 8) },
        unsafe { read(MAIL + 28) },
        unsafe { read(0x50109c00) == 0xa55a6996 }
    );
}
