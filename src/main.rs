#![no_std]
#![no_main]

use esp32_p4::{
    devices::{
        gt911::{Gt911, Kind, Tracker},
        rx8025t::Rx8025t,
    },
    hal,
};

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
    "fence.i",
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
    let mut backlight = unsafe { hal::gpio::Output::<23>::new_low() };
    backlight.set_low();
    timer.delay_us(100_000);
    let mut i2c = unsafe { hal::i2c::I2c::new(&mut timer) };
    let mut touch = loop {
        match Gt911::new(&mut i2c, &mut timer) {
            Ok(touch) => break touch,
            Err(error) => {
                let _ = writeln!(console, "GT911 init: {:?}", error);
                timer.delay_us(1_000_000);
            }
        }
    };
    let info = touch.info();
    let _ = writeln!(
        console,
        "GT911 READY addr=0x5d fw={:04x} config={} size={}x{} max={}",
        info.firmware, info.config_version, info.width, info.height, info.max_contacts
    );
    let mut tracker = Tracker::default();
    let mut frames = 0u32;
    let mut errors = 0u32;
    let mut heartbeat = timer.now_us();
    loop {
        match touch.read_frame(&mut i2c, &mut timer) {
            Ok(frame) => {
                if frame.is_some() {
                    frames = frames.wrapping_add(1);
                }
                for event in tracker.update(frame).into_iter().flatten() {
                    let kind = match event.kind {
                        Kind::Down => "down",
                        Kind::Move => "move",
                        Kind::Up => "up",
                    };
                    let _ = writeln!(
                        console,
                        "touch {} id={} x={} y={} size={} us={}",
                        kind,
                        event.point.id,
                        event.point.x,
                        event.point.y,
                        event.point.size,
                        event.timestamp_us
                    );
                }
            }
            Err(error) => {
                errors = errors.wrapping_add(1);
                if errors % 100 == 1 {
                    let _ = writeln!(console, "GT911 error: {:?}", error);
                }
            }
        }
        let now = timer.now_us();
        if now.wrapping_sub(heartbeat) & ((1 << 48) - 1) >= 5_000_000 {
            let _ = writeln!(
                console,
                "touch alive frames={} errors={} us={}",
                frames, errors, now
            );
            match Rx8025t::read(&mut i2c, &mut timer) {
                Ok(rtc) => {
                    let _ = write!(console, "RTC raw:");
                    for byte in rtc.raw {
                        let _ = write!(console, " {:02x}", byte);
                    }
                    let _ = writeln!(console);
                    let _ = writeln!(
                        console,
                        "RTC VLF={} VDET={} RESET={} TEST={} usable={}",
                        rtc.voltage_low(),
                        rtc.voltage_detected(),
                        rtc.stopped(),
                        rtc.test_mode(),
                        rtc.time_usable()
                    );
                    match rtc.datetime() {
                        Ok(t) => {
                            let _ = writeln!(
                                console,
                                "RTC {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                                t.year, t.month, t.day, t.hour, t.minute, t.second
                            );
                        }
                        Err(_) => {
                            let _ = writeln!(console, "RTC invalid calendar (BCD/date/weekday)");
                        }
                    }
                }
                Err(error) => {
                    let _ = writeln!(console, "RTC read: {:?}", error);
                }
            }
            heartbeat = now;
        }
        timer.delay_us(10_000);
    }
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
