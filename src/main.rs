#![no_std]
#![no_main]

use esp32_p4::hal;

use core::{arch::global_asm, fmt::Write, panic::PanicInfo};

global_asm!(
    ".section .text.entry,\"ax\"",
    ".global _start",
    ".option push",
    ".option norelax",
    "_start:",
    "csrci mstatus, 8", // Keep machine interrupts disabled until a trap handler exists.
    // Keep L2 SRAM clock on before moving the stack out of the 8-KiB TCM.
    "li t0, 0x500e5060",
    "lw t1, 0(t0)",
    "ori t1, t1, 1",
    "sw t1, 0(t0)",
    "fence rw, rw",
    "la sp, _stack_top",
    "la t0, _bss_start",
    "la t1, _bss_end",
    "1:",
    "bgeu t0, t1, 2f",
    "sw zero, 0(t0)",
    "addi t0, t0, 4",
    "j 1b",
    "2:",
    "call rust_main",
    "3: j 3b",
    ".option pop",
);

#[no_mangle]
extern "C" fn rust_main() -> ! {
    unsafe { hal::watchdog::disable() };
    let mut console = unsafe { hal::usb_serial::Console::new() };
    let _ = writeln!(console, "P4 rev1.3: ROM -> Rust OK");
    let mut timer = unsafe { hal::time::Timer::new() };
    let _ = writeln!(console, "SYSTIMER: XTAL/2.5, 16 MHz");
    // Keep the backlight off while probing shared peripheral control I2C.
    let mut backlight = unsafe { hal::gpio::Output::<23>::new_low() };
    backlight.set_low();
    timer.delay_us(100_000);
    let mut i2c = unsafe { hal::i2c::I2c::new(&mut timer) };
    loop {
        let _ = writeln!(console, "I2C1: SDA=7 SCL=8 100kHz scan");
        let mut found = 0;
        for address in 0x08..=0x77 {
            match i2c.probe(address, &mut timer) {
                Ok(()) => {
                    found += 1;
                    let _ = writeln!(console, "ACK 0x{:02x}", address);
                }
                Err(hal::i2c::Error::Nack) => {}
                Err(error) => {
                    let _ = writeln!(console, "0x{:02x}: {:?}", address, error);
                }
            }
        }
        let _ = writeln!(console, "I2C scan: {} addresses", found);
        // ES8311 ID registers do not support the block-read used by other chips.
        let mut codec_id = [0u8; 3];
        let mut codec_result = Ok(());
        for (index, reg) in (0xfd..=0xff).enumerate() {
            codec_result =
                i2c.write_read(0x18, &[reg], &mut codec_id[index..index + 1], &mut timer);
            if codec_result.is_err() {
                break;
            }
        }
        match codec_result {
            Ok(()) => {
                let _ = writeln!(
                    console,
                    "ES8311 0x18: {:02x} {:02x} {:02x}",
                    codec_id[0], codec_id[1], codec_id[2]
                );
            }
            Err(error) => {
                let _ = writeln!(console, "ES8311: {:?}", error);
            }
        }
        for (address, reg, len, name) in [
            (0x5d, &b"\x81\x40"[..], 4, "GT911"),
            (0x14, &b"\x81\x40"[..], 4, "GT911-alt"),
            (0x36, &b"\x30\x0a"[..], 2, "camera"),
            (0x32, &b"\x00"[..], 7, "RTC"),
        ] {
            let mut data = [0u8; 7];
            match i2c.write_read(address, reg, &mut data[..len], &mut timer) {
                Ok(()) => {
                    let _ = write!(console, "{} 0x{:02x}:", name, address);
                    for byte in &data[..len] {
                        let _ = write!(console, " {:02x}", byte);
                    }
                    let _ = writeln!(console);
                }
                Err(error) => {
                    let _ = writeln!(console, "{}: {:?}", name, error);
                }
            }
        }
        // Exercise a full RX FIFO using GT911's read-only ID/status area.
        let mut full_fifo = [0u8; 32];
        let fifo_result = i2c.write_read(0x5d, &[0x81, 0x40], &mut full_fifo, &mut timer);
        let fifo_ok = fifo_result.is_ok() && &full_fifo[..4] == b"911\0";
        let _ = writeln!(console, "RX32: {}", fifo_ok);
        // Separate write-only register pointer and read-only phases, with STOP.
        let mut split_id = [0u8; 4];
        let split_result = i2c
            .write_read(0x5d, &[0x81, 0x40], &mut [], &mut timer)
            .and_then(|()| i2c.write_read(0x5d, &[], &mut split_id, &mut timer));
        let split_ok = split_result.is_ok() && &split_id == b"911\0";
        let _ = writeln!(
            console,
            "write/read with STOP: {}",
            if split_ok { "PASS" } else { "FAIL" }
        );
        let absent_start = timer.now_us();
        let absent = i2c.probe(0x08, &mut timer);
        let _ = writeln!(
            console,
            "absent 0x08: {:?}, elapsed_us={}",
            absent,
            timer.now_us() - absent_start
        );
        let invalid = i2c.probe(0x80, &mut timer);
        let _ = writeln!(console, "invalid 0x80: {:?}", invalid);
        let _ = writeln!(console, "I2C diagnostic complete, us={}", timer.now_us());
        timer.delay_us(5_000_000);
    }
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
