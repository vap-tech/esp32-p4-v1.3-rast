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
    "la t0, _trap",
    "csrw mtvec, t0",
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
    ".balign 4",
    "_trap:",
    "csrr a0, mcause",
    "csrr a1, mepc",
    "csrr a2, mtval",
    "call rust_trap",
    "4: j 4b",
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
                        "DISPLAY READY VERTICAL 25% PWM20k duty={}",
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
    let mut csi: Option<hal::csi::Csi> = None;
    let mut psram = None;
    let mut framebuffer: Option<esp32_p4::devices::framebuffer::Framebuffer> = None;
    let mut scanout_active = false;
    let mut camera_live = false;
    let mut camera_controls = esp32_p4::devices::ov02c10::Controls::preview();
    loop {
        if let Some(command) = console.read_byte() {
            if matches!(command, b'+' | b'-') {
                if let Some(panel) = display.as_mut() {
                    let brightness = if command == b'+' {
                        panel.brightness().saturating_add(5).min(100)
                    } else {
                        panel.brightness().saturating_sub(5)
                    };
                    panel.set_brightness(brightness);
                    let _ = writeln!(
                        console,
                        "CMD {} BACKLIGHT {}% duty={}",
                        command as char,
                        panel.brightness(),
                        panel.pwm_duty()
                    );
                }
            } else if matches!(command, b'[' | b']' | b'{' | b'}' | b'<' | b'>') {
                if !camera_live {
                    let _ = writeln!(console, "CMD {} camera off; use V first", command as char);
                    continue;
                }
                let mut requested = camera_controls;
                match command {
                    b']' => {
                        requested.digital_q10 = requested.digital_q10.saturating_mul(2).min(8192)
                    }
                    b'[' => requested.digital_q10 = (requested.digital_q10 / 2).max(1024),
                    b'}' => {
                        requested.analogue_q4 = requested.analogue_q4.saturating_mul(2).min(248)
                    }
                    b'{' => requested.analogue_q4 = (requested.analogue_q4 / 2).max(16),
                    b'>' => {
                        requested.exposure_lines =
                            requested.exposure_lines.saturating_add(128).min(1156)
                    }
                    b'<' => {
                        requested.exposure_lines =
                            requested.exposure_lines.saturating_sub(128).max(4)
                    }
                    _ => {}
                }
                let result =
                    esp32_p4::devices::ov02c10::set_controls(&mut i2c, &mut timer, requested);
                if result.is_ok() {
                    camera_controls = requested;
                }
                let actual = esp32_p4::devices::ov02c10::read_controls(&mut i2c, &mut timer);
                let _ = writeln!(
                    console,
                    "CMD {} CAMERA_CONTROLS {:?} actual={:?}",
                    command as char, result, actual
                );
            } else if command == b'=' {
                let actual = esp32_p4::devices::ov02c10::read_controls(&mut i2c, &mut timer);
                let brightness = display.as_ref().map(|panel| panel.brightness());
                let _ = writeln!(
                    console,
                    "CMD = CAMERA_CONTROLS {:?} BACKLIGHT {:?}%",
                    actual, brightness
                );
            } else if matches!(command, b'V' | b'G') {
                if !scanout_active {
                    let _ = writeln!(console, "CMD {} use F first", command as char);
                    continue;
                }
                if let Some(receiver) = csi.as_mut() {
                    receiver.stop();
                }
                camera_live = false;
                use esp32_p4::devices::ov02c10 as sensor;
                let configured =
                    sensor::configure_window(&mut i2c, &mut timer, 1024, 600, command == b'G');
                if let Err(error) = configured {
                    let _ = sensor::stream(&mut i2c, &mut timer, false);
                    let _ = writeln!(console, "CMD {} sensor {:?}", command as char, error);
                    continue;
                }
                if let Err(error) = sensor::set_controls(&mut i2c, &mut timer, camera_controls) {
                    let _ = sensor::stream(&mut i2c, &mut timer, false);
                    let _ = writeln!(console, "CMD {} controls {:?}", command as char, error);
                    continue;
                }
                if csi.is_none() {
                    csi = Some(unsafe { hal::csi::Csi::new(&mut timer) });
                }
                let start = sensor::stream(&mut i2c, &mut timer, true);
                if start.is_err() {
                    let _ = writeln!(console, "CMD {} start {:?}", command as char, start);
                    continue;
                }
                timer.delay_us(100_000);
                let result = unsafe {
                    csi.as_mut()
                        .unwrap()
                        .start_rgb565(esp32_p4::devices::framebuffer::DMA_ADDRESS)
                };
                camera_live = result.is_ok();
                if !camera_live {
                    let _ = sensor::stream(&mut i2c, &mut timer, false);
                }
                let _ = writeln!(
                    console,
                    "CMD {} live={} {:?}",
                    command as char, camera_live, result
                );
            } else if command == b'O' {
                if let Some(receiver) = csi.as_mut() {
                    receiver.stop();
                }
                let _ = esp32_p4::devices::ov02c10::stream(&mut i2c, &mut timer, false);
                camera_live = false;
                let _ = writeln!(console, "CMD O camera stopped");
            } else if command == b'Q' {
                if let (Some(fb), Some(panel)) = (framebuffer.as_mut(), display.as_mut()) {
                    if camera_live {
                        csi.as_mut().unwrap().stop();
                        let _ = esp32_p4::devices::ov02c10::stream(&mut i2c, &mut timer, false);
                        camera_live = false;
                    }
                    for color in [0x0000, 0xf800, 0x07e0, 0x001f, 0xffff] {
                        fb.fill_rgb565(color);
                        timer.delay_us(100_000);
                        let _ = writeln!(
                            console,
                            "COLOR_PROBE memory={:04x} panel={:?}",
                            color,
                            panel.first_pixel(&mut timer)
                        );
                    }
                    for color in [0x0000, 0xf800, 0x07e0, 0x001f, 0xffff] {
                        fb.fill_rgb565_halfwords(color);
                        timer.delay_us(100_000);
                        let _ = writeln!(
                            console,
                            "COLOR_PROBE16 memory={:04x} panel={:?}",
                            color,
                            panel.first_pixel(&mut timer)
                        );
                    }
                    fb.color_check();
                    let _ = writeln!(console, "CMD Q color probe done");
                } else {
                    let _ = writeln!(console, "CMD Q use F first");
                }
            } else if command == b'E' {
                if let Some(fb) = framebuffer.as_mut() {
                    if camera_live {
                        csi.as_mut().unwrap().stop();
                        let _ = esp32_p4::devices::ov02c10::stream(&mut i2c, &mut timer, false);
                        camera_live = false;
                    }
                    fb.color_check();
                    let _ = writeln!(
                        console,
                        "CMD E color-check active={} guards={}",
                        scanout_active,
                        fb.guards_ok()
                    );
                } else {
                    let _ = writeln!(console, "CMD E use F first");
                }
            } else if command == b'F' {
                if camera_live {
                    csi.as_mut().unwrap().stop();
                    let _ = esp32_p4::devices::ov02c10::stream(&mut i2c, &mut timer, false);
                    camera_live = false;
                }

                if let Some(fb) = framebuffer.as_mut() {
                    fb.color_bars();
                } else {
                    let _ = writeln!(console, "FRAMEBUFFER initialize PSRAM");
                    let mut memory = match psram.take() {
                        Some(memory) => memory,
                        None => match unsafe { hal::psram::Psram::new(&mut timer) } {
                            Ok(memory) => memory,
                            Err(error) => {
                                let _ = writeln!(console, "CMD F PSRAM {:?}", error);
                                continue;
                            }
                        },
                    };
                    let _ = writeln!(console, "PSRAM {:?}", memory.info);
                    if let Err(error) = memory.check() {
                        let _ = writeln!(console, "CMD F memory {:?}", error);
                        continue;
                    }
                    framebuffer = Some(esp32_p4::devices::framebuffer::Framebuffer::new(memory));
                }
                if !scanout_active {
                    if let Some(panel) = display.as_mut() {
                        let result = unsafe {
                            panel.start_framebuffer(
                                esp32_p4::devices::framebuffer::DMA_ADDRESS,
                                &mut timer,
                            )
                        };
                        scanout_active = result.is_ok();
                        let _ = writeln!(console, "FRAMEBUFFER start {:?}", result);
                    }
                }
                let _ = writeln!(
                    console,
                    "CMD F active={} guards={}",
                    scanout_active,
                    framebuffer.as_ref().unwrap().guards_ok()
                );
            } else if command == b'S' {
                if let Some(panel) = display.as_mut() {
                    let _ = writeln!(
                        console,
                        "FRAMEBUFFER DMA {:08x?}",
                        panel.framebuffer_status()
                    );
                }
                if let Some(fb) = framebuffer.as_ref() {
                    let _ = writeln!(
                        console,
                        "FRAMEBUFFER guards={} pixels={:04x?}",
                        fb.guards_ok(),
                        fb.sample()
                    );
                }
                if let Some(receiver) = csi.as_mut() {
                    let _ = writeln!(console, "CAMERA LIVE {:?}", receiver.live_status());
                }
                let _ = writeln!(
                    console,
                    "CMD S active={} camera={}",
                    scanout_active, camera_live
                );
            } else if command == b'W' {
                if let Some(fb) = framebuffer.as_mut() {
                    let resume = camera_live;
                    if resume {
                        csi.as_mut().unwrap().stop();
                    }
                    fb.snapshot();
                    if resume {
                        // Sensor keeps streaming while only its receiver is paused.
                        camera_live = unsafe {
                            csi.as_mut()
                                .unwrap()
                                .start_rgb565(esp32_p4::devices::framebuffer::DMA_ADDRESS)
                        }
                        .is_ok();
                    }
                    let _ = writeln!(console, "FB_FULL 1024 600");
                    for y in 0..600 {
                        let _ = write!(console, "FBROW ");
                        for x in 0..1024 {
                            let _ = write!(console, "{:04x}", fb.snapshot_pixel(x, y).unwrap());
                        }
                        let _ = writeln!(console);
                    }
                    let _ = writeln!(console, "CMD W screenshot done live={}", camera_live);
                } else {
                    let _ = writeln!(console, "CMD W use F first");
                }
            } else if command == b'H' {
                if let Some(fb) = framebuffer.as_ref() {
                    let _ = writeln!(console, "FB_THUMB 128 75");
                    for y in 0..75 {
                        let _ = write!(console, "FBROW ");
                        for x in 0..128 {
                            let _ = write!(console, "{:04x}", fb.pixel(x * 8, y * 8).unwrap());
                        }
                        let _ = writeln!(console);
                    }
                }
                let _ = writeln!(console, "CMD H thumbnail done");
            } else if command == b'U' {
                if camera_live {
                    csi.as_mut().unwrap().stop();
                    let _ = esp32_p4::devices::ov02c10::stream(&mut i2c, &mut timer, false);
                    camera_live = false;
                }

                if scanout_active {
                    if let Some(panel) = display.as_mut() {
                        panel.stop_framebuffer();
                    }
                    scanout_active = false;
                }
                let _ = writeln!(console, "CMD U patterns restored");
            } else if command == b'P' {
                if framebuffer.is_some() {
                    let _ = writeln!(console, "CMD P framebuffer owns PSRAM; test skipped");
                    continue;
                }

                let _ = writeln!(console, "PSRAM initialize: register40MHz / AXI80MHz");
                if psram.is_none() {
                    match unsafe { hal::psram::Psram::new(&mut timer) } {
                        Ok(memory) => {
                            let _ = writeln!(console, "PSRAM {:?}", memory.info);
                            psram = Some(memory);
                        }
                        Err(error) => {
                            let _ = writeln!(console, "CMD P {:?}", error);
                            continue;
                        }
                    }
                }
                let _ = writeln!(console, "PSRAM memory check 4MiB");
                let _ = writeln!(console, "CMD P {:?}", psram.as_mut().unwrap().check());
            } else if matches!(command, b'a' | b'A' | b'o') {
                if camera_live && command != b'a' {
                    let _ = writeln!(console, "CMD {} camera live; O stops it", command as char);
                    continue;
                }

                camera_command(
                    command,
                    &mut csi,
                    &mut i2c,
                    &mut timer,
                    &mut console,
                    display.is_some(),
                );
            } else if scanout_active && !matches!(command, b'?' | b'q' | b'e' | b'i' | b'I') {
                let _ = writeln!(
                    console,
                    "CMD {} framebuffer active; U restores patterns",
                    command as char
                );
            } else if let Some(panel) = display.as_mut() {
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
    let _ = writeln!(console, "DISPLAY commands: +/- backlight5%; ]/[ digital gain; }}/{{ analogue gain; >/< exposure; = controls; Q panel RGB probe; E gray/RGB check; W full screenshot; H thumbnail; V camera-live; G camera-test-live; O camera-stop; F framebuffer; S fb-status; U patterns; P psram-test; a camera-info; A camera-test; o camera-scene; ? help; q status; B BIST-probe25s; L scanline-probe; e error-counter(clear); v/h/b patterns; i/I invert on/off; x/y/n mirror X/Y/none; c/C BGR/RGB; d/D display off/on; j/J idle on/off; k/f/N black/white/normal; s/w sleep/wake; t/u/T TE vblank/both/off; l TE line300; g idle-green; r/R soft/hard reset; 6/7 RGB565 at650/750; 8 RGB888 at750; z RGB565 at550");
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
        b'B' => {
            let _ = writeln!(
                console,
                "BIST PROBE: host video OFF for 25 seconds; automatic restoration follows"
            );
            let result = panel.probe_panel_bist(timer);
            let _ = writeln!(console, "BIST_RESULT {:?}", result);
            let _ = writeln!(
                console,
                "BIST_RESTORED duty={} PANEL {:?}",
                panel.pwm_duty(),
                panel.snapshot(timer)
            );
            return;
        }
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
        b'W' => panel.set_idle(false, timer),
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

#[repr(C, align(64))]
struct CameraBuffer {
    before: [u8; 64],
    pixels: [u8; esp32_p4::devices::ov02c10::FRAME_BYTES],
    after: [u8; 64],
}
static mut CAMERA_BUFFER: CameraBuffer = CameraBuffer {
    before: [0; 64],
    pixels: [0; esp32_p4::devices::ov02c10::FRAME_BYTES],
    after: [0; 64],
};

fn camera_command(
    command: u8,
    csi: &mut Option<hal::csi::Csi>,
    bus: &mut hal::i2c::I2c,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    phy_powered: bool,
) {
    use esp32_p4::devices::ov02c10 as camera;
    if command == b'a' {
        let _ = writeln!(console, "CAMERA {:?}", camera::snapshot(bus, timer));
        let _ = writeln!(console, "CMD a OK");
        return;
    }
    if !phy_powered {
        let _ = writeln!(console, "CMD {} error: LDO3 unavailable", command as char);
        return;
    }
    let _ = writeln!(
        console,
        "CAMERA capture 64x32 RAW10 pattern={}",
        command == b'A'
    );
    if let Err(error) = camera::configure_crop(bus, timer, command == b'A') {
        let _ = camera::stream(bus, timer, false);
        let _ = writeln!(console, "CMD {} sensor error {:?}", command as char, error);
        return;
    }
    if csi.is_none() {
        *csi = Some(unsafe { hal::csi::Csi::new(timer) });
    }
    let receiver = csi.as_mut().unwrap();
    // Single core, interrupts disabled. No Rust reference exists during DMA.
    let raw = core::ptr::addr_of_mut!(CAMERA_BUFFER);
    unsafe {
        raw.write(CameraBuffer {
            before: [0x5a; 64],
            pixels: [0xa5; camera::FRAME_BYTES],
            after: [0x5a; 64],
        });
    }
    let ptr = unsafe { core::ptr::addr_of_mut!((*raw).pixels).cast::<u8>() };
    let _ = writeln!(
        console,
        "CAMERA configured {:?} {:?}",
        camera::snapshot(bus, timer),
        receiver.status()
    );
    let start = camera::stream(bus, timer, true);
    if start.is_err() {
        receiver.stop();
        let _ = camera::stream(bus, timer, false);
        let _ = writeln!(console, "CMD {} stream error {:?}", command as char, start);
        return;
    }
    // Let the sensor emit its initial unmatched boundary packet, then capture
    // from a fresh bridge/input state. No USB logging during this interval.
    timer.delay_us(100_000);
    let result = unsafe {
        receiver.arm(
            ptr,
            camera::FRAME_BYTES,
            camera::WIDTH,
            camera::HEIGHT,
            timer,
        )
    };
    if let Err(error) = result {
        let _ = camera::stream(bus, timer, false);
        let _ = writeln!(console, "CMD {} arm error {:?}", command as char, error);
        return;
    }
    let (result, status) = receiver.finish(timer);
    let stop = camera::stream(bus, timer, false);
    // Observe hardware writes without copying the entire frame onto the stack.
    let mut guards = true;
    let mut changed = 0usize;
    let mut hash = 2166136261u32;
    let mut first = [0u8; 32];
    for i in 0..64 {
        guards &= unsafe {
            core::ptr::addr_of!((*raw).before)
                .cast::<u8>()
                .add(i)
                .read_volatile()
                == 0x5a
                && core::ptr::addr_of!((*raw).after)
                    .cast::<u8>()
                    .add(i)
                    .read_volatile()
                    == 0x5a
        };
    }
    for i in 0..camera::FRAME_BYTES {
        let byte = unsafe { ptr.add(i).read_volatile() };
        changed += usize::from(byte != 0xa5);
        hash = (hash ^ u32::from(byte)).wrapping_mul(16777619);
        if i < first.len() {
            first[i] = byte;
        }
    }
    let _ = writeln!(
        console,
        "CAMERA result={:?} start={:?} stop={:?} {:?}",
        result, start, stop, status
    );
    let _ = writeln!(
        console,
        "CAMERA DATA guards={} changed={} hash={:08x} first={:02x?}",
        guards, changed, hash, first
    );
    let _ = writeln!(console, "CMD {} CAMERA_DONE", command as char);
}

#[no_mangle]
extern "C" fn rust_trap(cause: u32, pc: u32, value: u32) -> ! {
    let mut console = unsafe { hal::usb_serial::Console::new() };
    let _ = writeln!(
        console,
        "TRAP cause={:08x} pc={:08x} value={:08x}",
        cause, pc, value
    );
    loop {
        core::hint::spin_loop();
    }
}
