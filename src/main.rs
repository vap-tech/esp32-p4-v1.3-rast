#![no_std]
#![no_main]

use esp32_p4::{
    devices::{
        gt911::{Gt911, Kind, Tracker},
        jd9165,
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
    let backlight_pin = unsafe { hal::gpio::Output::<23>::new_low() };
    let backlight = unsafe { hal::ledc::Pwm::new(backlight_pin) };
    timer.delay_us(100_000);
    let _ = writeln!(
        console,
        "DSI: init LDO3=2500mV, PLL ref=20MHz, 2 lanes x 550Mbps"
    );
    let mut display = match unsafe { hal::dsi::Dsi::new(&mut timer) } {
        Ok(dsi) => {
            let reset = unsafe { hal::gpio::Output::<27>::new_low() };
            match jd9165::Display::new(dsi, reset, backlight, &mut timer) {
                Ok(mut panel) => {
                    let _ = writeln!(
                        console,
                        "DISPLAY READY BER 5% PWM20k duty={}",
                        panel.pwm_duty()
                    );
                    let _ = writeln!(console, "PANEL {:?}", panel.snapshot(&mut timer));
                    Some(panel)
                }
                Err(error) => {
                    let _ = writeln!(console, "JD9165 init failed: {:?}", error);
                    None
                }
            }
        }
        Err(error) => {
            let _ = writeln!(console, "DSI init failed: {:?} {:?}", error, unsafe {
                hal::dsi::Dsi::status()
            });
            None
        }
    };
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
    display_help(&mut console);
    loop {
        if let Some(command) = console.read_byte() {
            if let Some(panel) = display.as_mut() {
                display_command(command, panel, &mut timer, &mut console);
            }
        }
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
            if let Some(panel) = display.as_mut() {
                let _ = writeln!(console, "DISPLAY host: {:?}", panel.host_status());
            }
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

fn display_help(console: &mut hal::usb_serial::Console) {
    let _ = writeln!(console, "DISPLAY commands: ? help; q status; L scanline-probe; e error-counter(clear); v/h/b patterns; i/I invert on/off; x/y/n mirror X/Y/none; c/C BGR/RGB; d/D display off/on; j/J idle on/off; k/f/N black/white/normal; s/w sleep/wake; t/u/T TE vblank/both/off; l TE line300; g idle-green; r/R soft/hard reset; 6/7 RGB565 at650/750; 8 RGB888 at750; z RGB565 at550");
}

fn display_command(
    command: u8,
    panel: &mut jd9165::Display,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) {
    use hal::dsi::{LaneRate, PixelFormat};
    use jd9165::{ColorOrder, Fill, Reset, TearEffect};
    let result = match command {
        b'?' => {
            display_help(console);
            return;
        }
        b'q' => {
            let _ = writeln!(console, "PANEL {:?}", panel.snapshot(timer));
            let _ = writeln!(
                console,
                "PIXEL {:?} IDLE_COLOR {:?}",
                panel.first_pixel(timer),
                panel.idle_color(timer)
            );
            return;
        }
        b'L' => {
            let _ = writeln!(console, "SCAN {:?}", panel.scanline(timer));
            return;
        }
        b'e' => {
            let _ = writeln!(
                console,
                "PANEL_ERRORS {:?}",
                panel.take_packet_errors(timer)
            );
            return;
        }
        b'v' => panel.set_pattern(hal::dsi::Pattern::Vertical),
        b'h' => panel.set_pattern(hal::dsi::Pattern::Horizontal),
        b'b' => panel.set_pattern(hal::dsi::Pattern::Ber),
        b'6' => panel.set_video_config(LaneRate::Mbps650, PixelFormat::Rgb565, timer),
        b'7' => panel.set_video_config(LaneRate::Mbps750, PixelFormat::Rgb565, timer),
        b'8' => panel.set_video_config(LaneRate::Mbps750, PixelFormat::Rgb888, timer),
        b'z' => panel.set_video_config(LaneRate::Mbps550, PixelFormat::Rgb565, timer),
        b'i' => panel.set_inverted(true, timer),
        b'I' => panel.set_inverted(false, timer),
        b'x' => panel.set_mirror(true, false, timer),
        b'y' => panel.set_mirror(false, true, timer),
        b'n' => panel.set_mirror(false, false, timer),
        b'c' => panel.set_color_order(ColorOrder::Bgr, timer),
        b'C' => panel.set_color_order(ColorOrder::Rgb, timer),
        b'd' => panel.set_display_on(false, timer),
        b'D' => panel.set_display_on(true, timer),
        b'j' => panel.set_idle(true, timer),
        b'J' => panel.set_idle(false, timer),
        b'k' => panel.set_test_fill(Some(Fill::Black), timer),
        b'f' => panel.set_test_fill(Some(Fill::White), timer),
        b'N' => panel.set_test_fill(None, timer),
        b's' => panel.sleep(timer),
        b'w' => panel.wake(timer),
        b't' => panel.set_tear_effect(TearEffect::Vblank, timer),
        b'u' => panel.set_tear_effect(TearEffect::VblankAndHblank, timer),
        b'T' => panel.set_tear_effect(TearEffect::Off, timer),
        b'l' => panel.set_tear_scanline(300, timer),
        b'g' => panel.set_idle_color(false, true, false, timer),
        b'r' => panel.reset(Reset::Software, timer),
        b'R' => panel.reset(Reset::Hardware, timer),
        _ => return,
    };
    let _ = writeln!(
        console,
        "CMD {} {:?} duty={} PANEL {:?}",
        char::from(command),
        result,
        panel.pwm_duty(),
        panel.snapshot(timer)
    );
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
