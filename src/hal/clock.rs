//! Board clock policy for ESP32-P4 rev1.3, 40 MHz crystal.
//! Register sequence follows ESP-IDF v6.1 clk_tree_ll / rtc_clk / regi2c_impl.
use super::time::Timer;
const ROOT: usize = 0x5011_1040;
const DIV: usize = 0x500e_6004;
const ANA: usize = 0x5012_4000;
fn read(a: usize) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
fn write(a: usize, v: u32) {
    unsafe { (a as *mut u32).write_volatile(v) }
}
fn modify(a: usize, mask: u32, value: u32) {
    write(a, (read(a) & !mask) | value);
}
fn wait(a: usize, mask: u32, value: u32, timer: &mut Timer) -> Result<(), &'static str> {
    let start = timer.now_us();
    while read(a) & mask != value {
        if timer.now_us().wrapping_sub(start) > 10_000 {
            return Err("clock timeout");
        }
    }
    Ok(())
}
fn update(timer: &mut Timer) -> Result<(), &'static str> {
    modify(DIV, 0, 1 << 4);
    wait(DIV, 1 << 4, 0, timer)
}
fn analog(reg: u32, data: u32, timer: &mut Timer) -> Result<(), &'static str> {
    wait(ANA, 1 << 25, 0, timer)?;
    write(ANA + 0x20, 1 << 11);
    write(ANA + 0x1c, 0);
    write(ANA, 0x67 | (reg << 8) | (data << 16) | (1 << 24));
    wait(ANA, 1 << 25, 0, timer)
}
/// Initialize before any peripheral DMA starts. Only for board revision 1.3.
///
/// # Safety
/// Caller owns clock tree and analog configuration bus; other HP core is parked.
pub unsafe fn init(timer: &mut Timer) -> Result<(), &'static str> {
    // Switch away from PLL before programming it; crystal remains stable.
    modify(ROOT, 3, 0);
    // Explicit integer CPU /2, MEM /1, SYS /1, APB /2.
    modify(DIV, 0x1fff_ffe0, 1 << 5);
    update(timer)?;
    modify(DIV + 4, 0xffff_ffff, 0);
    update(timer)?;
    modify(DIV + 8, 0xffff_ffff, 1 << 16);
    update(timer)?;
    modify(0x5011_50cc, 0, (1 << 23) | (1 << 27));
    modify(0x5011_50cc, 0, 1 << 17);
    let old_clock = read(0x5012_0000);
    modify(0x5012_0000, 0, 1 << 27);
    modify(ANA + 0x34, 0, 1);
    modify(0x500e_60bc, 1 << 3, 0);
    // ECO1+ feedback encoding: 40 MHz * 8 = 320 MHz.
    let result = (|| {
        analog(2, 0x50, timer)?;
        analog(3, 8, timer)?;
        analog(6, 0x73, timer)?;
        wait(0x500e_60bc, 1 << 2, 1 << 2, timer)?;
        timer.delay_us(10);
        Ok(())
    })();
    modify(0x500e_60bc, 0, 1 << 3);
    modify(0x5012_0000, 1 << 27, old_clock & (1 << 27));
    result?;
    modify(ROOT, 3, 1);
    #[cfg(target_arch = "riscv32")]
    {
        let set_ticks: unsafe extern "C" fn(u32) =
            unsafe { core::mem::transmute(0x4fc0_0044usize) };
        unsafe { set_ticks(160) };
        let start_us = timer.now_us();
        let start_cycles = super::time::cycle_count();
        timer.delay_us(1_000);
        let cycles = super::time::cycle_count().wrapping_sub(start_cycles);
        let elapsed = timer.now_us().wrapping_sub(start_us);
        let mhz = cycles / elapsed.max(1);
        if !(155..=165).contains(&mhz) {
            return Err("CPU frequency verification");
        }
    }
    Ok(())
}
