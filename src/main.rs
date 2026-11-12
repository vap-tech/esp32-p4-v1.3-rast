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

#[cfg(feature = "audio-lab")]
mod audio_lab;

mod arbitration_lab;
mod recorder;
mod web_server;

use core::{arch::global_asm, fmt::Write, panic::PanicInfo};

global_asm!(include_str!("boot.S"), l2_size = const hal::memory::L2_SIZE_REGISTER);

#[no_mangle]
extern "C" fn rust_memory_boot() {
    unsafe { hal::watchdog::disable() };
    let mut console = unsafe { hal::usb_serial::Console::new() };
    let _ = writeln!(
        console,
        "MEMORY BOOT CACHE {:x?}",
        hal::memory::cache_snapshot()
    );
    match unsafe { hal::memory::init_runtime_memory() } {
        Ok(()) => {
            let _ = writeln!(
                console,
                "MEMORY SRAM configured {}KiB; stack=16KiB work={}KiB",
                (hal::memory::INTERNAL_END - hal::memory::UPPER_BASE) / 1024,
                (hal::memory::STACK_START - hal::memory::internal_start()) / 1024
            );
        }
        Err(error) => {
            let _ = writeln!(console, "MEMORY BOOT FAILED {:?}", error);
            loop {
                core::hint::spin_loop();
            }
        }
    }
}

#[no_mangle]
extern "C" fn rust_main() -> ! {
    unsafe { hal::watchdog::disable() };
    let mut console = unsafe { hal::usb_serial::Console::new() };
    let _ = writeln!(console, "P4 rev1.3: ROM -> Rust OK");
    let mut timer = unsafe { hal::time::Timer::new() };
    let _ = writeln!(console, "SYSTIMER: XTAL/2.5, 16 MHz");
    if let Err(error) = unsafe { hal::clock::init(&mut timer) } {
        let _ = writeln!(console, "CLOCK INIT FAILED {}", error);
        loop {
            core::hint::spin_loop();
        }
    }
    clock_audit("boot", &mut timer, &mut console);
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
                        "DISPLAY READY backlight 25%, preparing framebuffer duty={}",
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
    let mut psram = None;
    let mut large_arena: Option<esp32_p4::memory::Arena<'static, esp32_p4::memory::External>> =
        None;
    // Boot checked the layout and reserved the stack; one arena owner per run.
    let mut internal_arena = unsafe { hal::memory::internal_arena() };
    let mut dma_arena = unsafe { hal::memory::dma_arena() };
    let mut framebuffer: Option<esp32_p4::devices::framebuffer::Framebuffer> = None;
    let mut scanout_active = false;
    // Keep backlight on: show the real transition while preparing DMA scanout.
    const BOOT_COLOR: u16 = 0x87e0; // Light green, RGB565.
    let boot_started = timer.now_us();
    let boot_result = (|| -> Result<(), &'static str> {
        let memory = unsafe { hal::psram::Psram::new(&mut timer) }.map_err(|_| "PSRAM init")?;
        let (display_memory, arena) = memory.split();
        large_arena = Some(arena);
        framebuffer = Some(esp32_p4::devices::framebuffer::Framebuffer::new_filled(
            display_memory,
            BOOT_COLOR,
        ));
        let panel = display.as_mut().ok_or("panel unavailable")?;
        unsafe { panel.start_framebuffer(esp32_p4::devices::framebuffer::DMA_ADDRESS, &mut timer) }
            .map_err(|_| "DMA start")?;
        scanout_active = true;
        Ok(())
    })();
    let _ = writeln!(
        console,
        "BOOT FRAMEBUFFER {:?} color={:04x} us={} guards={}",
        boot_result,
        BOOT_COLOR,
        timer.now_us().wrapping_sub(boot_started),
        framebuffer.as_ref().is_some_and(|fb| fb.guards_ok())
    );
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
    let mut camera_live = false;
    let mut camera_controls = esp32_p4::devices::ov02c10::Controls::preview();
    let mut command_input = esp32_p4::console::Input::default();
    let mut alarm_active = false;
    let mut alarm_poll = timer.now_us().wrapping_sub(1_000_000);
    let mut alarm_retry = timer.now_us();
    let mut clock_enabled = true;
    let mut clock_face = esp32_p4::clock_face::ClockFace::default();
    let mut clock_poll = timer.now_us().wrapping_sub(1_000_000);
    loop {
        let mut received = None;
        let mut frame_bytes = [0u8; 96];
        while let Some(byte) = console.read_byte() {
            if let Some(frame) = command_input.feed(byte, timer.now_us()) {
                received = Some(frame.map(|text| {
                    frame_bytes[..text.len()].copy_from_slice(text.as_bytes());
                    text.len()
                }));
                break;
            }
        }
        if let Some(frame) = received {
            let name = match frame {
                Ok(len) => core::str::from_utf8(&frame_bytes[..len]).unwrap(),
                Err(error) => {
                    let _ = writeln!(console, "COMMAND ERROR {}", error);
                    continue;
                }
            };
            let command = esp32_p4::console::action(name).unwrap_or(0);
            if matches!(command, b'Q' | b'E' | b'V' | b'G' | b'U') {
                clock_enabled = false;
            }
            if command == 0 {
                let mut normalized = [0; 96];
                let Some(line) = esp32_p4::console::route(name, &mut normalized) else {
                    let _ = writeln!(console, "COMMAND ERROR unknown command; use :help");
                    continue;
                };
                if line == "arbitration status" {
                    let _ = writeln!(
                        console,
                        "ARBITRATION STATE {:x?}",
                        hal::arbitration::snapshot()
                    );
                    let _ = writeln!(
                        console,
                        "ARBITRATION REGULATORS {:x?}",
                        hal::arbitration::regulator_snapshot()
                    );
                    let _ = writeln!(console, "ARBITRATION DONE Ok(())");
                } else if let Some(args) = line.strip_prefix("arbitration bench ") {
                    if camera_live
                        || !scanout_active
                        || alarm_active
                        || internal_arena.stats().used != 0
                        || large_arena.as_ref().is_none_or(|a| a.stats().used != 0)
                    {
                        let _=writeln!(console,"ARBITRATION DONE Err(requires idle arenas, active LCD, camera/alarm off)");
                        continue;
                    }
                    let result = arbitration_lab::run(
                        args,
                        arbitration_lab::Context {
                            fb: framebuffer.as_mut().unwrap(),
                            panel: display.as_mut().unwrap(),
                            timer: &mut timer,
                            console: &mut console,
                            internal: &internal_arena,
                            external: large_arena.as_ref().unwrap(),
                        },
                    );
                    internal_arena.reset();
                    large_arena.as_mut().unwrap().reset();
                    let _ = writeln!(console, "ARBITRATION DONE {:?}", result);
                } else if let Some(args) = line.strip_prefix("record ") {
                    let mut words = args.split_ascii_whitespace();
                    let path = words.next().unwrap_or("");
                    let seconds = words.next().and_then(|s| s.parse::<u32>().ok());
                    if path.is_empty()
                        || seconds.is_none()
                        || words.next().is_some()
                        || camera_live
                        || alarm_active
                    {
                        let _ = writeln!(console, "RECORD DONE Err(PATH SECONDS, no camera/alarm)");
                        continue;
                    }
                    let result = recorder::run(
                        path,
                        seconds.unwrap(),
                        recorder::Context {
                            bus: &mut i2c,
                            timer: &mut timer,
                            console: &mut console,
                            arena: &internal_arena,
                            touch: &mut touch,
                            panel: display.as_mut(),
                            ui: if scanout_active {
                                framebuffer.as_mut().map(|fb| (fb, &mut clock_face))
                            } else {
                                None
                            },
                        },
                    );
                    internal_arena.reset();
                    clock_face.invalidate();
                    tracker = Tracker::default();
                    let _ = writeln!(
                        console,
                        "RECORD DONE {:?} stack={:?}",
                        result,
                        hal::memory::stack_usage()
                    );
                } else if line == "clock-ui on" || line == "clock-ui off" {
                    clock_enabled = line.ends_with(" on");
                    clock_face.invalidate();
                    clock_poll = timer.now_us().wrapping_sub(1_000_000);
                    let _ = writeln!(console, "DISPLAY CLOCK DONE Ok({})", clock_enabled);
                } else if let Some(value) = line.strip_prefix("brightness ") {
                    match value.parse::<u8>().ok().filter(|n| *n <= 100) {
                        Some(percent) => {
                            if let Some(panel) = display.as_mut() {
                                panel.set_brightness(percent);
                                let _ =
                                    writeln!(console, "DISPLAY BRIGHTNESS DONE Ok({})", percent);
                            } else {
                                let _ =
                                    writeln!(console, "DISPLAY BRIGHTNESS DONE Err(no display)");
                            }
                        }
                        None => {
                            let _ = writeln!(console, "DISPLAY BRIGHTNESS DONE Err(range 0..100)");
                        }
                    }
                } else if let Some(command) = line.strip_prefix("rtc ") {
                    let result = rtc_command(command, &mut i2c, &mut timer);
                    match result {
                        Ok(ref snapshot) => {
                            let _ = writeln!(console, "RTC DATA raw={:02x?} date={:?} settings={:?} usable={} VLF={} VDET={} alarm={} timer={} update={}", snapshot.raw, snapshot.datetime(), snapshot.settings(), snapshot.time_usable(), snapshot.voltage_low(), snapshot.voltage_detected(), snapshot.alarm_pending(), snapshot.timer_pending(), snapshot.update_pending());
                        }
                        Err(ref error) => {
                            let _ = writeln!(console, "RTC ERROR {:?}", error);
                        }
                    }
                    let _ = writeln!(console, "RTC DONE {:?}", result.map(|_| ()));
                } else if let Some(seconds) = line.strip_prefix("web-run ") {
                    if let Ok(seconds @ 1..=86400) = seconds.parse::<u32>() {
                        if camera_live || dma_arena.stats().used != 0 {
                            let _ = writeln!(console, "WEB RUN DONE Err(state)");
                            continue;
                        }
                        let result = web_server::run(seconds, &dma_arena, &mut timer, &mut console);
                        dma_arena.reset();
                        let _ = writeln!(
                            console,
                            "WEB RUN DONE {:?} stack={:?}",
                            result,
                            hal::memory::stack_usage()
                        );
                    } else {
                        let _ = writeln!(console, "WEB RUN DONE Err(seconds)");
                    }
                } else if let Some(seconds) = line.strip_prefix("eth-run ") {
                    let seconds = seconds
                        .parse::<u32>()
                        .ok()
                        .filter(|s| (1..=600).contains(s));
                    if camera_live || dma_arena.stats().used != 0 || seconds.is_none() {
                        let _ = writeln!(console, "ETH RUN DONE Err(state/seconds)");
                        continue;
                    }
                    let result =
                        ethernet_run(seconds.unwrap(), &dma_arena, &mut timer, &mut console);
                    dma_arena.reset();
                    let _ = writeln!(
                        console,
                        "ETH RUN DONE {:?} stack={:?}",
                        result,
                        hal::memory::stack_usage()
                    );
                } else if line == "eth-probe" {
                    let result = ethernet_probe(&mut timer, &mut console);
                    let _ = writeln!(console, "ETH PROBE DONE {:?}", result);
                } else if line.starts_with("format-sd") {
                    if camera_live {
                        let _ = writeln!(console, "SD FORMAT DONE Err(camera active)");
                        continue;
                    }
                    if line != "format-sd" && line != "format-sd FAT32 ERASE" {
                        let _ = writeln!(console, "SD FORMAT DONE Err(use format-sd FAT32 ERASE)");
                        continue;
                    }
                    let erase = line == "format-sd FAT32 ERASE";
                    let result = sd_format(erase, &mut timer, &mut console);
                    let _ = writeln!(
                        console,
                        "SD FORMAT DONE {:?} stack={:?}",
                        result,
                        hal::memory::stack_usage()
                    );
                } else if line == "wav-lab"
                    || line == "wav-lab-stall"
                    || line == "wav-lab-control"
                    || line == "wav-lab-repeat"
                {
                    #[cfg(feature = "audio-lab")]
                    {
                        if camera_live || !scanout_active || dma_arena.stats().used != 0 {
                            let _ = writeln!(console, "WAV LAB DONE Err(state)");
                            continue;
                        }
                        let result = audio_lab::run(
                            framebuffer.as_mut().unwrap(),
                            &dma_arena,
                            &mut i2c,
                            &mut timer,
                            &mut console,
                            line,
                        );
                        dma_arena.reset();
                        let _ = writeln!(
                            console,
                            "WAV LAB DONE {:?} guards={} stack={:?}",
                            result,
                            framebuffer.as_ref().unwrap().guards_ok(),
                            hal::memory::stack_usage()
                        );
                    }
                    #[cfg(not(feature = "audio-lab"))]
                    {
                        let _ = writeln!(console, "WAV LAB DONE Err(build audio-lab)");
                    }
                } else if line == "mixed-stress" {
                    if camera_live || !scanout_active {
                        let _ = writeln!(console, "MIXED DONE Err(state)");
                        continue;
                    }
                    let result =
                        mixed_stress(framebuffer.as_mut().unwrap(), &mut timer, &mut console);
                    let _ = writeln!(
                        console,
                        "MIXED DONE {:?} guards={} stack={:?}",
                        result,
                        framebuffer.as_ref().unwrap().guards_ok(),
                        hal::memory::stack_usage()
                    );
                } else if line == "sd-stress" {
                    if camera_live || !scanout_active {
                        let _ = writeln!(console, "SD STRESS DONE Err(state)");
                        continue;
                    }
                    if dma_arena.stats().used != 0 {
                        let _ = writeln!(console, "SD STRESS DONE Err(arena busy)");
                        continue;
                    }
                    let result = sd_stress(&dma_arena, &mut timer, &mut console);
                    dma_arena.reset();
                    let _ = writeln!(
                        console,
                        "SD STRESS DONE {:?} guards={} stack={:?}",
                        result,
                        framebuffer.as_ref().unwrap().guards_ok(),
                        hal::memory::stack_usage()
                    );
                } else if line == "l2-probe" {
                    if camera_live || framebuffer.is_none() {
                        let _ = writeln!(console, "L2 PROBE DONE Err(state)");
                        continue;
                    }
                    let result = unsafe { hal::memory::probe_l2() };
                    let _ = writeln!(console, "L2 PROBE DONE {:?}", result);
                } else if line == "dcache on"
                    || line == "dcache off"
                    || line == "l2 on"
                    || line == "l2 off"
                {
                    if camera_live {
                        let _ = writeln!(console, "CACHE DONE Err(camera active)");
                        continue;
                    }
                    let result = unsafe {
                        if line.starts_with("dcache") {
                            hal::memory::set_data_cache(line == "dcache on")
                        } else {
                            hal::memory::set_l2_cache(line == "l2 on")
                        }
                    };
                    let _ = writeln!(
                        console,
                        "CACHE DONE {:?} {:?}",
                        result,
                        hal::memory::cache_snapshot()
                    );
                } else if line == "icache on" || line == "icache off" {
                    let result = unsafe { hal::memory::set_instruction_cache(line == "icache on") };
                    let _ = writeln!(
                        console,
                        "ICACHE DONE {:?} {:?}",
                        result,
                        hal::memory::cache_snapshot()
                    );
                } else if line == "clock" {
                    clock_audit("runtime", &mut timer, &mut console);
                    let _ = writeln!(console, "CLOCK DONE Ok(())");
                } else if line == "memory" {
                    let _ = writeln!(console, "MEMORY CACHE {:x?}", hal::memory::cache_snapshot());
                    let _ = writeln!(console, "MEMORY STACK {:?}", hal::memory::stack_usage());
                    let _ = writeln!(console, "MEMORY WORK {:?}", internal_arena.stats());
                    let _ = writeln!(console, "MEMORY DMA {:?}", dma_arena.stats());
                    let _ = writeln!(console, "MEMORY LAYOUT {:x?}", hal::memory::layout());
                    if let Some(arena) = large_arena.as_ref() {
                        let _ = writeln!(console, "MEMORY EXTERNAL {:?}", arena.stats());
                    }
                    let _ = writeln!(console, "MEMORY DONE Ok(())");
                } else if line == "memory-bench" {
                    // Initialize/split PSRAM without enabling display scanout.
                    if large_arena.is_none() {
                        let memory = match psram.take() {
                            Some(memory) => Ok(memory),
                            None => unsafe { hal::psram::Psram::new(&mut timer) },
                        };
                        match memory {
                            Ok(memory) => {
                                let (display_memory, arena) = memory.split();
                                large_arena = Some(arena);
                                framebuffer =
                                    Some(esp32_p4::devices::framebuffer::Framebuffer::new(
                                        display_memory,
                                    ));
                            }
                            Err(error) => {
                                let _ = writeln!(console, "MEMORY DONE Err({:?})", error);
                                continue;
                            }
                        }
                    }
                    let before_hash = framebuffer.as_ref().unwrap().checksum();
                    let result = memory_bench(
                        &internal_arena,
                        large_arena.as_ref().unwrap(),
                        &mut timer,
                        &mut console,
                    );
                    // No outstanding allocations: reclaim both scratch arenas.
                    internal_arena.reset();
                    large_arena.as_mut().unwrap().reset();
                    let after_hash = framebuffer.as_ref().unwrap().checksum();
                    let result = result.and(
                        if before_hash == after_hash && framebuffer.as_ref().unwrap().guards_ok() {
                            Ok(())
                        } else {
                            Err(hal::memory::Error::Mismatch {
                                address: esp32_p4::devices::framebuffer::CPU_ADDRESS,
                                expected: before_hash,
                                actual: after_hash,
                            })
                        },
                    );
                    let _ = writeln!(
                        console,
                        "MEMORY FRAMEBUFFER hash_before={:08x} hash_after={:08x} unchanged={}",
                        before_hash,
                        after_hash,
                        before_hash == after_hash
                    );
                    let _ = writeln!(
                        console,
                        "MEMORY DONE {:?} display_active={} guards={} stack={:?}",
                        result,
                        scanout_active,
                        framebuffer.as_ref().unwrap().guards_ok(),
                        hal::memory::stack_usage()
                    );
                } else if line == "memory-test" {
                    if framebuffer.is_some() {
                        let _ = writeln!(
                            console,
                            "MEMORY DONE Err(display owns PSRAM; reset before full test)"
                        );
                        continue;
                    }
                    if psram.is_none() {
                        match unsafe { hal::psram::Psram::new(&mut timer) } {
                            Ok(memory) => psram = Some(memory),
                            Err(error) => {
                                let _ = writeln!(console, "MEMORY DONE Err({:?})", error);
                                continue;
                            }
                        }
                    }
                    let memory = psram.as_mut().unwrap();
                    let _ = writeln!(
                        console,
                        "MEMORY PSRAM full test bytes={} {:?}",
                        memory.info.bytes, memory.info
                    );
                    let start = timer.now_us();
                    let result = memory.check_all(|pass, offset| {
                        let _ = writeln!(
                            console,
                            "MEMORY TEST pass={} checked_MiB={}",
                            pass + 1,
                            offset / (1024 * 1024)
                        );
                    });
                    let _ = writeln!(
                        console,
                        "MEMORY DONE {:?} elapsed_us={} stack={:?}",
                        result,
                        timer.now_us().wrapping_sub(start) & ((1 << 48) - 1),
                        hal::memory::stack_usage()
                    );
                } else if let Some(path) = line.strip_prefix("image ") {
                    clock_enabled = false;
                    if camera_live {
                        let _ = writeln!(
                            console,
                            "MEDIA IMAGE DONE Err(camera active; use :camera stop)"
                        );
                        continue;
                    }
                    if display.is_none() {
                        let _ = writeln!(console, "MEDIA IMAGE DONE Err(display unavailable)");
                        continue;
                    }
                    if framebuffer.is_none() {
                        let memory = match psram.take() {
                            Some(memory) => Ok(memory),
                            None => unsafe { hal::psram::Psram::new(&mut timer) },
                        };
                        match memory {
                            Ok(mut memory) => {
                                if let Err(error) = memory.check() {
                                    let _ = writeln!(console, "MEDIA IMAGE DONE Err({:?})", error);
                                    continue;
                                }
                                let (display_memory, arena) = memory.split();
                                large_arena = Some(arena);
                                framebuffer =
                                    Some(esp32_p4::devices::framebuffer::Framebuffer::new(
                                        display_memory,
                                    ));
                            }
                            Err(error) => {
                                let _ = writeln!(console, "MEDIA IMAGE DONE Err({:?})", error);
                                continue;
                            }
                        }
                    }
                    let fb = framebuffer.as_mut().unwrap();
                    let result = sd_image(path, fb, &mut timer, &mut console);
                    if result.is_ok() && !scanout_active {
                        let result = unsafe {
                            display.as_mut().unwrap().start_framebuffer(
                                esp32_p4::devices::framebuffer::DMA_ADDRESS,
                                &mut timer,
                            )
                        };
                        scanout_active = result.is_ok();
                        let _ = writeln!(console, "MEDIA SCANOUT {:?}", result);
                    }
                    let _ = writeln!(
                        console,
                        "MEDIA IMAGE DONE {:?} active={} guards={}",
                        result,
                        scanout_active,
                        fb.guards_ok()
                    );
                } else if let Some(path) = line.strip_prefix("jpeg-idct-bench ") {
                    if camera_live || !scanout_active || framebuffer.is_none() {
                        let _=writeln!(console,"JPEG IDCT DONE Err(requires active framebuffer, camera off; use --image first)");
                        continue;
                    }
                    let result = jpeg_idct_bench(
                        path,
                        framebuffer.as_mut().unwrap(),
                        &mut timer,
                        &mut console,
                    );
                    let _ = writeln!(
                        console,
                        "JPEG IDCT DONE {:?} guards={} stack={:?}",
                        result,
                        framebuffer.as_ref().unwrap().guards_ok(),
                        hal::memory::stack_usage()
                    );
                } else if let Some(path) = line.strip_prefix("jpeg-profile ") {
                    if camera_live || !scanout_active || framebuffer.is_none() {
                        let _=writeln!(console,"JPEG PROFILE DONE Err(requires active framebuffer, camera off; use --image first)");
                        continue;
                    }
                    let result = jpeg_profile(
                        path,
                        framebuffer.as_mut().unwrap(),
                        &mut timer,
                        &mut console,
                    );
                    let _ = writeln!(
                        console,
                        "JPEG PROFILE DONE {:?} guards={} stack={:?}",
                        result,
                        framebuffer.as_ref().unwrap().guards_ok(),
                        hal::memory::stack_usage()
                    );
                } else if let Some(path) = line.strip_prefix("jpeg-bench ") {
                    if camera_live || !scanout_active || framebuffer.is_none() {
                        let _=writeln!(console,"JPEG BENCH DONE Err(requires active framebuffer, camera off; use --image first)");
                        continue;
                    }
                    let result = jpeg_bench(
                        path,
                        framebuffer.as_mut().unwrap(),
                        &mut timer,
                        &mut console,
                    );
                    let _ = writeln!(
                        console,
                        "JPEG BENCH DONE {:?} guards={} stack={:?}",
                        result,
                        framebuffer.as_ref().unwrap().guards_ok(),
                        hal::memory::stack_usage()
                    );
                } else if let Some(path) = line
                    .strip_prefix("play ")
                    .or_else(|| line.strip_prefix("play-stall "))
                {
                    let result = sd_audio(
                        path,
                        &mut i2c,
                        &mut timer,
                        &mut console,
                        line.starts_with("play-stall "),
                        if clock_enabled && scanout_active && !camera_live {
                            framebuffer.as_mut().map(|fb| (fb, &mut clock_face))
                        } else {
                            None
                        },
                        None,
                    );
                    let _ = writeln!(console, "MEDIA PLAY DONE {:?}", result);
                } else {
                    let _ = writeln!(console, "MEDIA ERROR see docs/console.md");
                }
            } else if command == b'?' {
                display_help(&mut console);
            } else if command == 27 {
                let _ = writeln!(console, "MEDIA PLAY DONE Ok(idle)");
            } else if matches!(command, b'+' | b'-') {
                if let Some(panel) = display.as_mut() {
                    let brightness = if command == b'+' {
                        panel.brightness().saturating_add(5).min(100)
                    } else {
                        panel.brightness().saturating_sub(5)
                    };
                    panel.set_brightness(brightness);
                    let _ = writeln!(
                        console,
                        "COMMAND {} BACKLIGHT {}% duty={}",
                        esp32_p4::console::action_name(command),
                        panel.brightness(),
                        panel.pwm_duty()
                    );
                }
            } else if matches!(command, b'[' | b']' | b'{' | b'}' | b'<' | b'>') {
                if !camera_live {
                    let _ = writeln!(
                        console,
                        "COMMAND {} camera off; use :camera start first",
                        esp32_p4::console::action_name(command)
                    );
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
                    "COMMAND {} CAMERA_CONTROLS {:?} actual={:?}",
                    esp32_p4::console::action_name(command),
                    result,
                    actual
                );
            } else if command == b'=' {
                let actual = esp32_p4::devices::ov02c10::read_controls(&mut i2c, &mut timer);
                let brightness = display.as_ref().map(|panel| panel.brightness());
                let _ = writeln!(
                    console,
                    "COMMAND camera controls CAMERA_CONTROLS {:?} BACKLIGHT {:?}%",
                    actual, brightness
                );
            } else if matches!(command, b'V' | b'G') {
                if !scanout_active {
                    let _ = writeln!(
                        console,
                        "COMMAND {} use :display framebuffer start first",
                        esp32_p4::console::action_name(command)
                    );
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
                    let _ = writeln!(
                        console,
                        "COMMAND {} sensor {:?}",
                        esp32_p4::console::action_name(command),
                        error
                    );
                    continue;
                }
                if let Err(error) = sensor::set_controls(&mut i2c, &mut timer, camera_controls) {
                    let _ = sensor::stream(&mut i2c, &mut timer, false);
                    let _ = writeln!(
                        console,
                        "COMMAND {} controls {:?}",
                        esp32_p4::console::action_name(command),
                        error
                    );
                    continue;
                }
                if csi.is_none() {
                    csi = Some(unsafe { hal::csi::Csi::new(&mut timer) });
                }
                let start = sensor::stream(&mut i2c, &mut timer, true);
                if start.is_err() {
                    let _ = writeln!(
                        console,
                        "COMMAND {} start {:?}",
                        esp32_p4::console::action_name(command),
                        start
                    );
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
                    "COMMAND {} live={} {:?}",
                    esp32_p4::console::action_name(command),
                    camera_live,
                    result
                );
            } else if command == b'O' {
                if let Some(receiver) = csi.as_mut() {
                    receiver.stop();
                }
                let _ = esp32_p4::devices::ov02c10::stream(&mut i2c, &mut timer, false);
                camera_live = false;
                let _ = writeln!(console, "COMMAND camera stop camera stopped");
            } else if command == b'p' {
                let result = audio_tone(&mut i2c, &mut timer, &mut console);
                let _ = writeln!(console, "COMMAND audio tone {:?}", result);
            } else if command == b'!' {
                let result = sd_config_probe(&mut timer, &mut console);
                let _ = writeln!(console, "COMMAND sd config-create {:?}", result);
            } else if command == b'Z' {
                let result = sd_fat_probe(&mut timer, &mut console);
                let _ = writeln!(console, "COMMAND sd list {:?}", result);
            } else if command == b'Y' {
                let result = sdmmc_info(&mut timer, &mut console);
                let _ = writeln!(console, "COMMAND sd capabilities {:?}", result);
            } else if command == b'#' {
                let result = sdmmc_multi_write_probe(&mut timer, &mut console);
                let _ = writeln!(console, "COMMAND sd test multi-write {:?}", result);
            } else if command == b'@' {
                let result = sdmmc_write_probe(&mut timer, &mut console);
                let _ = writeln!(console, "COMMAND sd test write {:?}", result);
            } else if command == b'm' {
                let result = sdmmc_multi_probe(&mut timer, &mut console);
                let _ = writeln!(console, "COMMAND sd benchmark read {:?}", result);
            } else if command == b'M' {
                let result = sdmmc_probe(&mut timer, &mut console);
                let _ = writeln!(console, "COMMAND sd probe {:?}", result);
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
                    let _ = writeln!(console, "COMMAND display test rgb color probe done");
                } else {
                    let _ = writeln!(
                        console,
                        "COMMAND display test rgb use :display framebuffer start first"
                    );
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
                        "COMMAND display test chart color-check active={} guards={}",
                        scanout_active,
                        fb.guards_ok()
                    );
                } else {
                    let _ = writeln!(
                        console,
                        "COMMAND display test chart use :display framebuffer start first"
                    );
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
                                let _ = writeln!(
                                    console,
                                    "COMMAND display framebuffer start PSRAM {:?}",
                                    error
                                );
                                continue;
                            }
                        },
                    };
                    let _ = writeln!(console, "PSRAM {:?}", memory.info);
                    if let Err(error) = memory.check() {
                        let _ = writeln!(
                            console,
                            "COMMAND display framebuffer start memory {:?}",
                            error
                        );
                        continue;
                    }
                    let (display_memory, arena) = memory.split();
                    large_arena = Some(arena);
                    framebuffer = Some(esp32_p4::devices::framebuffer::Framebuffer::new(
                        display_memory,
                    ));
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
                    "COMMAND display framebuffer start active={} guards={}",
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
                    "COMMAND display framebuffer status active={} camera={}",
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
                    let _ = writeln!(
                        console,
                        "COMMAND display screenshot screenshot done live={}",
                        camera_live
                    );
                } else {
                    let _ = writeln!(
                        console,
                        "COMMAND display screenshot use :display framebuffer start first"
                    );
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
                let _ = writeln!(console, "COMMAND display thumbnail thumbnail done");
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
                let _ = writeln!(
                    console,
                    "COMMAND display framebuffer stop patterns restored"
                );
            } else if command == b'P' {
                if framebuffer.is_some() {
                    let _ = writeln!(
                        console,
                        "COMMAND memory psram-test framebuffer owns PSRAM; test skipped"
                    );
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
                            let _ = writeln!(console, "COMMAND memory psram-test {:?}", error);
                            continue;
                        }
                    }
                }
                let _ = writeln!(console, "PSRAM memory check 4MiB");
                let _ = writeln!(
                    console,
                    "COMMAND memory psram-test {:?}",
                    psram.as_mut().unwrap().check()
                );
            } else if matches!(command, b'a' | b'A' | b'o') {
                if camera_live && command != b'a' {
                    let _ = writeln!(
                        console,
                        "COMMAND {} camera live; :camera stop stops it",
                        esp32_p4::console::action_name(command)
                    );
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
                    "COMMAND {} framebuffer active; :display framebuffer stop restores patterns",
                    esp32_p4::console::action_name(command)
                );
            } else if let Some(panel) = display.as_mut() {
                display_command(command, panel, &mut timer, &mut console);
            }
        }
        if timer.now_us().wrapping_sub(alarm_poll) >= 1_000_000 {
            alarm_poll = timer.now_us();
            if let Ok(s) = Rx8025t::read(&mut i2c, &mut timer) {
                if s.time_usable() && s.alarm_enabled() && s.alarm_pending() && !alarm_active {
                    alarm_active = true;
                    alarm_retry = timer.now_us();
                    let _ = writeln!(console, "ALARM FIRED RTC AF=true AIE=true");
                    if scanout_active && !camera_live {
                        if let Some(fb) = framebuffer.as_mut() {
                            esp32_p4::clock_face::alarm_button(fb, true);
                        }
                    }
                }
            }
        }
        if alarm_active && timer.now_us() >= alarm_retry && scanout_active && !camera_live {
            clock_enabled = true;
            if let Some(fb) = framebuffer.as_mut() {
                let result = sd_audio(
                    "SATIE.WAV",
                    &mut i2c,
                    &mut timer,
                    &mut console,
                    false,
                    Some((fb, &mut clock_face)),
                    Some((&mut touch, &mut alarm_active)),
                );
                let _ = writeln!(console, "ALARM AUDIO {:?} active={}", result, alarm_active);
                if !alarm_active {
                    esp32_p4::clock_face::alarm_button(fb, false);
                }
                // EOF repeats; SD/audio failure leaves touch active and retries slowly.
                alarm_retry = timer.now_us() + if result.is_ok() { 250_000 } else { 5_000_000 };
            }
            tracker = Tracker::default();
        }
        if clock_enabled
            && scanout_active
            && !camera_live
            && timer.now_us().wrapping_sub(clock_poll) >= 1_000_000
        {
            clock_poll = timer.now_us();
            let date = Rx8025t::read(&mut i2c, &mut timer)
                .ok()
                .filter(|s| s.time_usable())
                .and_then(|s| s.datetime().ok());
            if let Some(fb) = framebuffer.as_mut() {
                let started = timer.now_us();
                let changed = clock_face.draw(fb, date);
                let _ = writeln!(
                    console,
                    "CLOCK DRAW glyphs={} us={} valid={}",
                    changed,
                    timer.now_us().wrapping_sub(started),
                    date.is_some()
                );
            }
        }
        match touch.read_frame(&mut i2c, &mut timer) {
            Ok(frame) => {
                if frame.is_some() {
                    frames = frames.wrapping_add(1);
                }
                for event in tracker.update(frame).into_iter().flatten() {
                    if alarm_active
                        && event.kind == Kind::Down
                        && esp32_p4::clock_face::stop_hit(event.point.x, event.point.y)
                    {
                        match Rx8025t::acknowledge_alarm(&mut i2c, &mut timer) {
                            Ok(()) => {
                                alarm_active = false;
                                if let Some(fb) = framebuffer.as_mut() {
                                    esp32_p4::clock_face::alarm_button(fb, false);
                                }
                                let _ = writeln!(console, "ALARM ACK RTC AF cleared; AIE retained");
                            }
                            Err(error) => {
                                let _ = writeln!(console, "ALARM ACK ERROR {:?}", error);
                            }
                        }
                    }
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
    let _ = writeln!(
        console,
        "CONSOLE v1: :command [arguments] + newline; ESC stops active jobs"
    );
    for (name, _) in esp32_p4::console::entries(esp32_p4::console::COMMANDS) {
        let _ = writeln!(console, "  {}", name);
    }
    for (name, _) in esp32_p4::console::entries(esp32_p4::console::ROUTES) {
        let _ = writeln!(console, "  {} [arguments]", name);
    }
    let _ = writeln!(console, "CONSOLE HELP END");
}

#[inline(never)]
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
        "COMMAND {} {:?} duty={} PANEL {:?}",
        esp32_p4::console::action_name(command),
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

#[inline(never)]
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
        let _ = writeln!(console, "COMMAND camera info OK");
        return;
    }
    if !phy_powered {
        let _ = writeln!(
            console,
            "COMMAND {} error: LDO3 unavailable",
            esp32_p4::console::action_name(command)
        );
        return;
    }
    let _ = writeln!(
        console,
        "CAMERA capture 64x32 RAW10 pattern={}",
        command == b'A'
    );
    if let Err(error) = camera::configure_crop(bus, timer, command == b'A') {
        let _ = camera::stream(bus, timer, false);
        let _ = writeln!(
            console,
            "COMMAND {} sensor error {:?}",
            esp32_p4::console::action_name(command),
            error
        );
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
        let _ = writeln!(
            console,
            "COMMAND {} stream error {:?}",
            esp32_p4::console::action_name(command),
            start
        );
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
        let _ = writeln!(
            console,
            "COMMAND {} arm error {:?}",
            esp32_p4::console::action_name(command),
            error
        );
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
    let _ = writeln!(
        console,
        "COMMAND {} CAMERA_DONE",
        esp32_p4::console::action_name(command)
    );
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

// A periodic 1kHz sine at 16kHz, duplicated to both PCM16 slots, peak -18dBFS.
static AUDIO_TONE: [u32; 512] = {
    let sine: [i16; 16] = [
        0, 1567, 2896, 3784, 4096, 3784, 2896, 1567, 0, -1567, -2896, -3784, -4096, -3784, -2896,
        -1567,
    ];
    let mut frames = [0; 512];
    let mut i = 0;
    while i < frames.len() {
        let sample = sine[i % 16] as u16 as u32;
        frames[i] = sample | (sample << 16);
        i += 1;
    }
    frames
};

enum AudioError {
    Codec(hal::i2c::Error),
    I2s(hal::i2s::Error),
}
impl core::fmt::Debug for AudioError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Codec(error) => f.debug_tuple("Codec").field(error).finish(),
            Self::I2s(error) => f.debug_tuple("I2s").field(error).finish(),
        }
    }
}

#[inline(never)]
fn audio_tone(
    bus: &mut hal::i2c::I2c,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), AudioError> {
    use esp32_p4::devices::es8311::Es8311;
    let mut pa = unsafe { hal::gpio::Output::<11>::new_low() };
    let mut tx = unsafe { hal::i2s::Tx::new(&AUDIO_TONE, timer) }.map_err(AudioError::I2s)?;
    let result = (|| {
        // Old working capture enabled I2S clocks before opening the codec.
        tx.start(timer).map_err(AudioError::I2s)?;
        let mut codec = Es8311::new(bus, timer).map_err(AudioError::Codec)?;
        let _ = writeln!(console, "AUDIO ES8311 {:?}", codec.snapshot(bus, timer));
        let play = (|| {
            timer.delay_us(50_000);
            let before = tx.status();
            let _ = writeln!(
                console,
                "AUDIO PLAY PA=GPIO11 1000Hz 16000Hz PCM16 stereo/16bit slots DAC=-21dB peak=4096/32768 {:?}",
                before
            );
            pa.set_high();
            codec.mute(bus, timer, false).map_err(AudioError::Codec)?;
            let _ = writeln!(
                console,
                "AUDIO PADS [DOUT,WS,BCLK,MCLK] high/4096,edges={:?} PA={} codec={:?}",
                tx.pin_activity(),
                pa.level(),
                codec.snapshot(bus, timer)
            );
            let own = tx.capture(9, timer).map_err(AudioError::I2s)?;
            let _ = writeln!(
                console,
                "AUDIO RX OWN raw={:x} desc={:08x} words={:08x?}",
                own.dma,
                own.descriptor,
                &own.words[..16]
            );
            codec
                .reference_output(bus, timer, true)
                .map_err(AudioError::Codec)?;
            let reference = tx.capture(48, timer).map_err(AudioError::I2s)?;
            let _ = writeln!(
                console,
                "AUDIO RX CODEC raw={:x} desc={:08x} words={:08x?}",
                reference.dma,
                reference.descriptor,
                &reference.words[..16]
            );
            codec
                .reference_output(bus, timer, false)
                .map_err(AudioError::Codec)?;
            timer.delay_us(2_000_000);
            let after = tx.status();
            let _ = writeln!(
                console,
                "AUDIO SENT {:?} fifo_delta={} bck_delta={}",
                after,
                after.fifo.wrapping_sub(before.fifo),
                after.bck.wrapping_sub(before.bck)
            );
            if after.dma & (1 << 2) != 0 || after.dma & (1 << 1) == 0 {
                return Err(AudioError::I2s(hal::i2s::Error::Dma(after.dma)));
            }
            Ok(())
        })();
        let mute = codec.mute(bus, timer, true).map_err(AudioError::Codec);
        timer.delay_us(20_000);
        pa.set_low();
        play.and(mute)
    })();
    pa.set_low();
    let stop = tx.stop(timer).map_err(AudioError::I2s);
    let _ = writeln!(console, "AUDIO STOP PA={} {:?}", pa.level(), tx.status());
    result.and(stop)
}

type FsProbeError =
    esp32_p4::storage::Error<esp32_p4::storage::Error<esp32_p4::storage::sd::Error>>;

const BOARD_CONFIG: &[u8] = b"# ESP32-P4 board configuration\n[display]\nbrightness_percent=25\n[camera]\nenabled=false\n[storage]\nbus_width=4\nclock_hz=50000000\n";

#[inline(never)]
fn sd_format(
    erase: bool,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), FsProbeError> {
    use esp32_p4::storage::{
        fat32::Fat32,
        format,
        sd::{Error as SdError, SdDevice},
        Error, Partition,
    };
    let transport_error = |error| Error::Device(Error::Device(SdError::Card(error)));
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(transport_error)?;
    card.configure_bus(timer).map_err(transport_error)?;
    let plan = format::Plan::new(card.info.sectors).map_err(Error::Unsupported)?;
    let _ = writeln!(
        console,
        "SD FORMAT PLAN sectors={} start={} cluster_bytes={} FAT_sectors={}",
        plan.sectors,
        plan.start,
        u32::from(plan.sectors_per_cluster) * 512,
        plan.fat_sectors
    );
    if !erase {
        let _ = writeln!(
            console,
            "SD FORMAT PREVIEW; erase SD: :format-sd FAT32 ERASE"
        );
        return Ok(());
    }
    let volume_id = timer.now_us() as u32;
    let started = timer.now_us();
    {
        let mut device = SdDevice::new(card, timer);
        let scratch = unsafe { sd_probe_buffer() };
        let mut next_report = 0;
        format::quick_format(&mut device, scratch, volume_id, |done, total| {
            if done >= next_report || done == total {
                let _ = writeln!(console, "SD FORMAT metadata_sectors={}/{}", done, total);
                next_report = done.saturating_add(8192);
            }
        })
        .map_err(Error::Device)?;
        let partition = Partition::mbr(device, 0).map_err(Error::Device)?;
        let mut fs = Fat32::mount(partition)?;
        let _ = writeln!(
            console,
            "SD FORMAT MOUNT clusters={}",
            fs.geometry().clusters
        );
        let mut entries = 0;
        fs.read_dir("/", |_| {
            entries += 1;
            true
        })?;
        if entries != 0 {
            return Err(Error::Invalid("root not empty"));
        }
    }
    let _ = writeln!(
        console,
        "SD FORMAT metadata_us={}",
        timer.now_us() - started
    );
    if !sd_config_probe(timer, console)? {
        return Err(Error::Invalid("formatted CONFIG.TXT verification"));
    }
    Ok(())
}

#[inline(never)]
fn sd_config_probe(
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<bool, FsProbeError> {
    use esp32_p4::storage::{
        fat32::Fat32,
        sd::{Error as SdError, SdDevice},
        Error, Partition,
    };
    let transport_error = |error| Error::Device(Error::Device(SdError::Card(error)));
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(transport_error)?;
    card.configure_bus(timer).map_err(transport_error)?;
    let partition = Partition::mbr(SdDevice::new(card, timer), 0).map_err(Error::Device)?;
    let mut fs = Fat32::mount(partition)?;
    match fs.create_new("CONFIG.TXT", BOARD_CONFIG) {
        Ok(()) => {
            let _ = writeln!(console, "FAT32 CONFIG CREATED bytes={}", BOARD_CONFIG.len());
        }
        Err(Error::AlreadyExists) => {
            let _ = writeln!(console, "FAT32 CONFIG EXISTS (not overwritten)");
        }
        Err(error) => return Err(error),
    }
    // Destroy both filesystem caches and mount from the physical device again.
    let mut fs = Fat32::mount(fs.into_inner())?;
    let mut file = fs.open("CONFIG.TXT")?;
    let mut actual = [0u8; BOARD_CONFIG.len()];
    let n = file.read(&mut actual)?;
    let matches = file.len() as usize == BOARD_CONFIG.len()
        && n == BOARD_CONFIG.len()
        && actual == BOARD_CONFIG;
    let _ = writeln!(
        console,
        "FAT32 CONFIG REMOUNT bytes={} matches={}\n{}",
        n,
        matches,
        core::str::from_utf8(&actual[..n]).unwrap_or("<not UTF-8>")
    );
    Ok(matches)
}

#[inline(never)]
fn sd_fat_probe(
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<bool, FsProbeError> {
    use esp32_p4::storage::{
        fat32::Fat32,
        sd::{Error as SdError, SdDevice},
        Error, Partition,
    };
    let transport_error = |error| Error::Device(Error::Device(SdError::Card(error)));
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(transport_error)?;
    card.configure_bus(timer).map_err(transport_error)?;
    let _ = writeln!(console, "FAT32 CARD {:?}", card.info);
    let started = timer.now_us();
    let result = {
        let partition = Partition::mbr(SdDevice::new(card, timer), 0).map_err(Error::Device)?;
        let _ = writeln!(console, "FAT32 PARTITION start={}", partition.start());
        let mut fs = Fat32::mount(partition)?;
        let _ = writeln!(console, "FAT32 MOUNT {:?}", fs.geometry());
        let mut chosen = None;
        let mut parent = None;
        let mut directories = [None; 4];
        let mut directory_count = 0;
        let mut count = 0;
        fs.read_dir("/", |entry| {
            let _ = writeln!(
                console,
                "FAT32 ENTRY {} {} bytes={}",
                if entry.is_directory() { "DIR" } else { "FILE" },
                entry.name,
                entry.size
            );
            if !entry.is_directory()
                && entry.size > 0
                && chosen
                    .is_none_or(|old: esp32_p4::storage::fat32::DirEntry| entry.size > old.size)
            {
                chosen = Some(entry);
            }
            if entry.is_directory() && directory_count < directories.len() {
                directories[directory_count] = Some(entry.name);
                directory_count += 1;
            }
            count += 1;
            count < 64
        })?;
        for name in directories.into_iter().flatten() {
            let mut path = short_path::Path::new();
            let _ = write!(path, "{}", name);
            let mut count = 0;
            fs.read_dir(path.as_str(), |entry| {
                let _ = writeln!(
                    console,
                    "FAT32 ENTRY {}/{} bytes={} dir={}",
                    name,
                    entry.name,
                    entry.size,
                    entry.is_directory()
                );
                if !entry.is_directory()
                    && entry.size > 0
                    && chosen.is_none_or(|old| entry.size > old.size)
                {
                    chosen = Some(entry);
                    parent = Some(name);
                }
                count += 1;
                count < 16
            })?;
        }
        let matched = if let Some(entry) = chosen {
            let mut path = short_path::Path::new();
            if let Some(parent) = parent {
                let _ = write!(path, "{}/", parent);
            }
            let _ = write!(path, "{}", entry.name);
            let mut file = fs.open(path.as_str())?;
            // Read at most 1 MiB twice with different chunk boundaries. This
            // exercises full batches, partial edges, rewind and EOF on small files.
            let bytes = file.len().min(1_048_576);
            let buffer = unsafe { sd_probe_buffer() };
            let mut reference = None;
            for chunk in [8192usize, 777] {
                file.seek(0)?;
                let mut remaining = bytes as usize;
                let mut hash = 0x811c9dc5u32;
                while remaining != 0 {
                    let requested = chunk.min(remaining);
                    let n = file.read(&mut buffer[..requested])?;
                    if n == 0 {
                        return Ok(false);
                    }
                    if remaining == bytes as usize {
                        let _ = writeln!(console, "FAT32 FIRST {:02x?}", &buffer[..n.min(32)]);
                    }
                    for &byte in &buffer[..n] {
                        hash = (hash ^ u32::from(byte)).wrapping_mul(0x01000193);
                    }
                    remaining -= n;
                }
                let _ = writeln!(
                    console,
                    "FAT32 READ file={} size={} checked={} chunk={} hash={:08x}",
                    path.as_str(),
                    file.len(),
                    bytes,
                    chunk,
                    hash
                );
                if reference.is_some_and(|value| value != hash) {
                    return Ok(false);
                }
                reference = Some(hash);
            }
            true
        } else {
            let _ = writeln!(
                console,
                "FAT32 no nonempty regular file in first 64 root entries"
            );
            true
        };
        let stats = fs.into_inner().into_inner().stats();
        let _ = writeln!(console, "FAT32 BLOCK_IO {:?}", stats);
        matched
    };
    let _ = writeln!(
        console,
        "FAT32 read-only probe total_us={}",
        timer.now_us().wrapping_sub(started) & ((1 << 48) - 1)
    );
    Ok(result)
}

// Fixed-size formatting buffer for an 8.3 path; no heap and no dependencies.
mod short_path {
    pub struct Path {
        bytes: [u8; 26],
        len: usize,
    }
    impl Path {
        pub fn new() -> Self {
            Self {
                bytes: [0; 26],
                len: 0,
            }
        }
        pub fn as_str(&self) -> &str {
            core::str::from_utf8(&self.bytes[..self.len]).unwrap()
        }
    }
    impl core::fmt::Write for Path {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            if self.len + s.len() > self.bytes.len() {
                return Err(core::fmt::Error);
            }
            self.bytes[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
            self.len += s.len();
            Ok(())
        }
    }
}

#[inline(never)]
fn sdmmc_probe(
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<bool, hal::sdmmc::Error> {
    let _ = writeln!(
        console,
        "SDMMC init slot0 GPIO39..44, native SD, read-only probe"
    );
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }?;
    let _ = writeln!(console, "SDMMC {:?}", card.info);
    let mut sector = [0u8; 512];
    card.read_sector(0, &mut sector, timer)?;
    let mbr = sector;
    if sector[510..] != [0x55, 0xaa] {
        let _ = writeln!(console, "SDMMC invalid MBR signature; stopping probe");
        return Ok(false);
    }
    let partition = u32::from_le_bytes(sector[454..458].try_into().unwrap());
    let _ = writeln!(
        console,
        "SDMMC 1bit/400kHz MBR signature=55aa partition={}",
        partition
    );
    card.set_four_bit(timer)?;
    card.read_sector(0, &mut sector, timer)?;
    if sector != mbr {
        let _ = writeln!(
            console,
            "SDMMC MBR mismatch between 1bit and 4bit; stopping probe"
        );
        return Ok(false);
    }
    let _ = writeln!(
        console,
        "SDMMC 4bit/25MHz MBR matches, capacity={} bytes",
        card.info.sectors * 512
    );
    card.read_sector(partition, &mut sector, timer)?;
    let _ = writeln!(console, "SDMMC partition LBA={} signature={:02x?} label={:?} fs={:?} bytes_per_sector={} sectors_per_cluster={}",
        partition, &sector[510..], core::str::from_utf8(&sector[71..82]), core::str::from_utf8(&sector[82..90]),
        u16::from_le_bytes([sector[11],sector[12]]), sector[13]);
    if sector[510..] != [0x55, 0xaa] || u64::from(partition) + 2048 > card.info.sectors {
        let _ = writeln!(
            console,
            "SDMMC invalid partition boot sector or probe range"
        );
        return Ok(false);
    }
    let mut previous = None;
    for pass in 0..2 {
        if pass == 1 {
            card.configure_bus(timer)?;
            let _ = writeln!(console, "SDMMC negotiated {:?}", card.info);
            card.read_sector(0, &mut sector, timer)?;
            if sector != mbr {
                let _ = writeln!(console, "SDMMC MBR mismatch after speed change");
                return Ok(false);
            }
        }
        let start = timer.now_us();
        let mut checksum = 0x811c9dc5u32;
        // 1MiB, CMD17 per sector; baseline includes DMA setup/cache sync overhead.
        for lba in partition..partition + 2048 {
            card.read_sector(lba, &mut sector, timer)?;
            for chunk in sector.as_chunks::<4>().0 {
                checksum = (checksum ^ u32::from_le_bytes(*chunk)).wrapping_mul(0x01000193);
            }
        }
        let us = timer.now_us().wrapping_sub(start) & ((1 << 48) - 1);
        let _ = writeln!(
            console,
            "SDMMC READ pass={} clock_hz={} sectors=2048 us={} KiB_s={} checksum={:08x}",
            pass,
            card.info.clock_hz,
            us,
            1_024_000_000 / us.max(1),
            checksum
        );
        if previous.is_some_and(|old| old != checksum) {
            let _ = writeln!(console, "SDMMC read checksum mismatch; stopping probe");
            return Ok(false);
        }
        previous = Some(checksum);
    }
    let _ = writeln!(
        console,
        "SDMMC repeated 1MiB reads match; registers={:08x?}",
        card.registers()
    );
    Ok(true)
}

// Used only by synchronous console diagnostics, never by an interrupt.
// Reuse the retired 8 KiB boot stack, never concurrently with boot or audio.
// Callers are synchronous console diagnostics and initialize every byte here.
unsafe fn sd_probe_buffer() -> &'static mut [u8; hal::sdmmc::MAX_TRANSFER] {
    let ptr = 0x4ff2_6000 as *mut [u8; hal::sdmmc::MAX_TRANSFER];
    unsafe {
        ptr.cast::<u8>().write_bytes(0, hal::sdmmc::MAX_TRANSFER);
        &mut *ptr
    }
}

#[inline(never)]
fn sdmmc_multi_probe(
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<bool, hal::sdmmc::Error> {
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }?;
    card.configure_bus(timer)?;
    let _ = writeln!(console, "SD_MULTI {:?}", card.info);
    let buffer = unsafe { sd_probe_buffer() };
    let mut single = [0u8; 512];
    // Exercise both descriptors and nonzero LBAs, including the card's last block.
    for lba in [0, 2048, (card.info.sectors - 16) as u32] {
        card.read_sectors(lba, buffer, timer)?;
        for i in 0..16 {
            card.read_sector(lba + i, &mut single, timer)?;
            if single != buffer[i as usize * 512..(i as usize + 1) * 512] {
                let _ = writeln!(console, "SD_MULTI byte mismatch lba={}", lba + i);
                return Ok(false);
            }
        }
    }
    let _ = writeln!(console, "SD_MULTI head/middle/tail byte comparisons match");
    let mut baseline = None;
    for sectors in [1usize, 2, 8, 16] {
        let start = timer.now_us();
        let mut checksum = 0x811c9dc5u32;
        for offset in (0..2048u32).step_by(sectors) {
            let bytes = &mut buffer[..sectors * 512];
            card.read_sectors(2048 + offset, bytes, timer)?;
            for chunk in bytes.as_chunks::<4>().0 {
                checksum = (checksum ^ u32::from_le_bytes(*chunk)).wrapping_mul(0x01000193);
            }
        }
        let us = timer.now_us().wrapping_sub(start) & ((1 << 48) - 1);
        let _ = writeln!(
            console,
            "SD_MULTI READ burst_sectors={} bytes=1048576 us={} KiB_s={} checksum={:08x}",
            sectors,
            us,
            1_024_000_000 / us.max(1),
            checksum
        );
        if baseline.is_some_and(|value| value != checksum) {
            let _ = writeln!(console, "SD_MULTI checksum mismatch");
            return Ok(false);
        }
        baseline = Some(checksum);
    }
    Ok(true)
}

#[inline(never)]
fn sdmmc_multi_write_probe(
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<bool, hal::sdmmc::Error> {
    const LEN: usize = 9 * 512; // Cross the 4096B descriptor boundary.
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }?;
    card.configure_bus(timer)?;
    let lba = (card.info.sectors - 9) as u32;
    let original = &mut [0u8; LEN];
    card.read_sectors(lba, original, timer)?;
    let buffer = unsafe { sd_probe_buffer() };
    let bytes = &mut buffer[..LEN];
    let pattern = |i: usize| (i as u8).wrapping_mul(73) ^ ((i >> 8) as u8) ^ 0xa5;
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = pattern(i);
    }
    let start = timer.now_us();
    let checked = (|| {
        card.write_sectors(lba, bytes, timer)?;
        card.read_sectors(lba, bytes, timer)?;
        Ok(bytes
            .iter()
            .enumerate()
            .all(|(i, byte)| *byte == pattern(i)))
    })();
    let _ = writeln!(
        console,
        "SD_MULTI WRITE_TEST lba={} sectors=9 result={:?} us={}",
        lba,
        checked,
        timer.now_us().wrapping_sub(start) & ((1 << 48) - 1)
    );
    // A failed stream may need reinitialization before restoring the RAM backup.
    if checked.is_err() {
        card = unsafe { hal::sdmmc::Sdmmc::new(timer) }?;
        card.configure_bus(timer)?;
    }
    // Always restore after a failed trial as well. Power loss is not atomic.
    card.write_sectors(lba, original, timer)?;
    card.read_sectors(lba, bytes, timer)?;
    let restored = bytes == original;
    let _ = writeln!(
        console,
        "SD_MULTI WRITE_RESTORE lba={} sectors=9 matches={}",
        lba, restored
    );
    checked.map(|matched| matched && restored)
}

#[inline(never)]
fn sdmmc_write_probe(
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<bool, hal::sdmmc::Error> {
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }?;
    card.configure_bus(timer)?;
    let _ = writeln!(console, "SDMMC write bus {:?}", card.info);
    let lba = (card.info.sectors - 1) as u32;
    let mut original = [0u8; 512];
    card.read_sector(lba, &mut original, timer)?;
    let pattern: [u8; 512] =
        core::array::from_fn(|i| (i as u8).wrapping_mul(73) ^ ((i >> 3) as u8) ^ 0xa5);
    let mut actual = [0u8; 512];
    let start = timer.now_us();
    let checked = (|| {
        card.write_sector(lba, &pattern, timer)?;
        card.read_sector(lba, &mut actual, timer)?;
        Ok(actual == pattern)
    })();
    let _ = writeln!(
        console,
        "SDMMC WRITE_TEST lba={} result={:?} us={}",
        lba,
        checked,
        timer.now_us().wrapping_sub(start) & ((1 << 48) - 1)
    );
    // Always attempt restoration, including when the trial write/read failed.
    card.write_sector(lba, &original, timer)?;
    card.read_sector(lba, &mut actual, timer)?;
    let restored = actual == original;
    let _ = writeln!(
        console,
        "SDMMC WRITE_RESTORE lba={} matches={}",
        lba, restored
    );
    checked.map(|matched| matched && restored)
}

#[inline(never)]
fn sdmmc_info(
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), hal::sdmmc::Error> {
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }?;
    let _ = writeln!(console, "SD_INFO {:?}", card.info);
    let scr = card.read_scr(timer)?;
    let _ = writeln!(console, "SD_SCR raw={:02x?}", scr);
    let bits = u64::from_be_bytes(scr);
    let _ = writeln!(console, "SD_SCR structure={} sd_spec={} spec3={} spec4={} specx={} bus_width_mask={:x} CMD23={} CMD20={}",
        bits>>60, (bits>>56)&15, (bits>>47)&1, (bits>>42)&1, (bits>>38)&15,
        (bits>>48)&15, (bits>>33)&1, (bits>>32)&1);
    let status = card.read_sd_status(timer)?;
    let _ = writeln!(console, "SD_STATUS raw={:02x?}", status);
    let _ = writeln!(console, "SD_STATUS bus_width_code={} speed_class_code={} performance_move_MB_s={} AU_code={} UHS_grade={}",
        status[0]>>6, status[8], status[9], status[10]>>4, status[14]>>4);
    let csd = card.info.csd;
    let classes = (csd[2] >> 20) & 0xfff;
    let _ = writeln!(
        console,
        "SD_CSD tran_speed_code={:02x} command_classes={:03x} permanent_WP={} temporary_WP={}",
        csd[3] & 255,
        classes,
        (csd[0] >> 13) & 1,
        (csd[0] >> 12) & 1
    );
    if classes & (1 << 10) != 0 && (bits >> 56) & 15 >= 1 {
        let switch = card.check_speed(timer)?;
        let _ = writeln!(console, "SD_CMD6_CHECK raw={:02x?}", switch);
        let support = u16::from_be_bytes([switch[12], switch[13]]);
        let _ = writeln!(console, "SD_CMD6_CHECK max_current_mA={} access_modes={:04x} default={} high_speed={} SDR50={} SDR104={} DDR50={} check_result={} status_version={}",
            u16::from_be_bytes([switch[0],switch[1]]), support, support&1!=0, support&2!=0,
            support&4!=0, support&8!=0, support&16!=0, switch[16]&15, switch[17]);
    } else {
        let _ = writeln!(console, "SD_CMD6_CHECK unsupported by declared SCR/CSD");
    }
    let _ = writeln!(
        console,
        "SD_INFO unchanged width={} clock_hz={} voltage=3300mV (no CMD11, no CMD6 switch)",
        card.info.width, card.info.clock_hz
    );
    Ok(())
}

#[inline(never)]
fn sd_image(
    path: &str,
    fb: &mut esp32_p4::devices::framebuffer::Framebuffer,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), esp32_p4::media::Error<FsProbeError>> {
    use esp32_p4::{
        media::{jpeg, Error},
        storage::{
            fat32::Fat32,
            sd::{Error as SdError, SdDevice},
            Partition,
        },
    };
    let started = timer.now_us();
    let transport = |e| {
        Error::Io(esp32_p4::storage::Error::Device(
            esp32_p4::storage::Error::Device(SdError::Card(e)),
        ))
    };
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(transport)?;
    card.configure_bus(timer).map_err(transport)?;
    let partition = Partition::mbr(SdDevice::new(card, timer), 0)
        .map_err(|e| Error::Io(esp32_p4::storage::Error::Device(e)))?;
    let mut fs = Fat32::mount(partition).map_err(Error::Io)?;
    let mut file = fs.open(path).map_err(Error::Io)?;
    let _ = writeln!(
        console,
        "MEDIA IMAGE START path={} bytes={} CPU baseline JPEG -> RGB565",
        path,
        file.len()
    );
    let info = jpeg::decode(&mut file, 1024, 600, |x, y, pixels| {
        fb.write_run(x, y, pixels)
    })?;
    fb.finish_writes();
    let elapsed = file.with_timer(|timer| timer.now_us().wrapping_sub(started) & ((1 << 48) - 1));
    let _ = writeln!(
        console,
        "MEDIA JPEG {:?} us={} pixels={:04x?}",
        info,
        elapsed,
        fb.sample()
    );
    Ok(())
}

enum PlaybackError {
    Media(esp32_p4::media::Error<FsProbeError>),
    Audio(AudioError),
}
impl core::fmt::Debug for PlaybackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Media(error) => f.debug_tuple("Media").field(error).finish(),
            Self::Audio(error) => f.debug_tuple("Audio").field(error).finish(),
        }
    }
}
impl From<esp32_p4::media::Error<FsProbeError>> for PlaybackError {
    fn from(error: esp32_p4::media::Error<FsProbeError>) -> Self {
        Self::Media(error)
    }
}

#[inline(never)]
fn sd_audio(
    path: &str,
    bus: &mut hal::i2c::I2c,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    stall: bool,
    mut clock: Option<(
        &mut esp32_p4::devices::framebuffer::Framebuffer,
        &mut esp32_p4::clock_face::ClockFace,
    )>,
    mut alarm: Option<(&mut Gt911, &mut bool)>,
) -> Result<(), PlaybackError> {
    use esp32_p4::{
        devices::es8311::Es8311,
        media::{wav::Wave, Error},
        storage::{
            fat32::Fat32,
            sd::{Error as SdError, SdDevice},
            Partition,
        },
    };
    let transport = |e| {
        PlaybackError::Media(Error::Io(esp32_p4::storage::Error::Device(
            esp32_p4::storage::Error::Device(SdError::Card(e)),
        )))
    };
    let io = |e| PlaybackError::Media(Error::Io(e));
    let audio = |e| PlaybackError::Audio(AudioError::I2s(e));
    let codec_error = |e| PlaybackError::Audio(AudioError::Codec(e));
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(transport)?;
    card.configure_bus(timer).map_err(transport)?;
    let partition = Partition::mbr(SdDevice::new(card, timer), 0)
        .map_err(|e| io(esp32_p4::storage::Error::Device(e)))?;
    let mut fs = Fat32::mount(partition).map_err(io)?;
    let mut file = fs.open(path).map_err(io)?;
    let wave = Wave::parse(&mut file)?;
    if wave.sample_rate != 16000 {
        return Err(Error::Unsupported("playback clock currently 16000 Hz; use SATIE.WAV").into());
    }
    let mut pa = unsafe { hal::gpio::Output::<11>::new_low() };
    let mut tx = file
        .with_timer(|timer| unsafe { hal::i2s::Tx::new(&[0], timer) })
        .map_err(audio)?;
    let mut codec = None;
    let result = (|| {
        // Keep initial silence running while the codec is configured, then reprime
        // a parked DMA ring with actual samples before opening the amplifier.
        file.with_timer(|timer| tx.start(timer)).map_err(audio)?;
        codec = Some(
            file.with_timer(|timer| Es8311::new(bus, timer))
                .map_err(codec_error)?,
        );
        file.with_timer(|timer| tx.stop(timer)).map_err(audio)?;
        // Reinitialize clocks/descriptors without resetting the configured codec.
        tx = file
            .with_timer(|timer| unsafe { hal::i2s::Tx::new_stream(timer) })
            .map_err(audio)?;
        let mut frames = [0u32; 512];
        let mut total_frames = 0u32;
        let mut last_audio = None;
        for i in 0..hal::i2s::STREAM_BUFFERS {
            let n = wave.read_frames(&mut file, &mut frames)?;
            total_frames += n as u32;
            if n != 0 {
                last_audio = Some(i);
            }
            file.with_timer(|timer| tx.stream_refill(i, &frames, timer))
                .map_err(audio)?;
        }
        let _ = writeln!(
            console,
            "MEDIA PLAY START path={} rate={} channels={} data={} DAC=-21dB ESC=stop",
            path, wave.sample_rate, wave.channels, wave.data_bytes
        );
        file.with_timer(|timer| tx.start(timer)).map_err(audio)?;
        file.with_timer(|timer| codec.as_mut().unwrap().mute(bus, timer, false))
            .map_err(codec_error)?;
        pa.set_high();
        if stall {
            file.with_timer(|timer| timer.delay_us(200_000));
        }
        let mut index = 0;
        let mut refill_count = 0u32;
        let mut max_read_us = 0;
        let mut waiting = file.with_timer(|timer| timer.now_us());
        let mut touch_due = waiting;
        let mut clock_due = waiting;
        let mut clock_updates = 0u32;
        let mut max_clock_us = 0u64;
        loop {
            if console.read_byte() == Some(27) {
                if let Some((_, active)) = alarm.as_mut() {
                    if file
                        .with_timer(|timer| Rx8025t::acknowledge_alarm(bus, timer))
                        .is_ok()
                    {
                        **active = false;
                    }
                }
                let _ = writeln!(console, "MEDIA PLAY STOP requested");
                break;
            }
            let ready = file.with_timer(|timer| tx.stream_ready(index, timer));
            if let Err(ref error) = ready {
                let _ = writeln!(
                    console,
                    "MEDIA DMA ERROR index={} refills={} frames={} {:?} {:?}",
                    index,
                    refill_count,
                    total_frames,
                    error,
                    tx.status()
                );
            }
            if !ready.map_err(audio)? {
                if file
                    .with_timer(|timer| timer.now_us())
                    .wrapping_sub(touch_due)
                    >= 20_000
                {
                    touch_due = file.with_timer(|timer| timer.now_us());
                    if let Some((touch, active)) = alarm.as_mut() {
                        let hit = file.with_timer(|timer| alarm_stop_touch(touch, bus, timer));
                        if hit {
                            **active = false;
                            let _ = writeln!(console, "ALARM ACK RTC AF cleared; AIE retained");
                            break;
                        }
                    }
                }
                if file.with_timer(|timer| timer.now_us().wrapping_sub(waiting) & ((1 << 48) - 1))
                    > 200_000
                {
                    return Err(audio(hal::i2s::Error::Timeout("stream DMA release")));
                }
                continue;
            }
            // Once EOF is known, drain through the final data descriptor. A full
            // silent ring follows it, giving time to mute before stale data repeats.
            if file.position() == wave.data_offset + wave.data_bytes && last_audio == Some(index) {
                break;
            }
            let start = file.with_timer(|timer| timer.now_us());
            let n = wave.read_frames(&mut file, &mut frames)?;
            total_frames += n as u32;
            if n != 0 {
                last_audio = Some(index);
            }
            file.with_timer(|timer| tx.stream_refill(index, &frames, timer))
                .map_err(audio)?;
            max_read_us = max_read_us
                .max(file.with_timer(|timer| timer.now_us().wrapping_sub(start) & ((1 << 48) - 1)));
            index = (index + 1) % hal::i2s::STREAM_BUFFERS;
            refill_count += 1;
            waiting = file.with_timer(|timer| timer.now_us());
            // Refill audio first. While the four DMA blocks play independently,
            // use the resulting headroom for one bounded RTC/render operation.
            if waiting.wrapping_sub(clock_due) >= 1_000_000 {
                clock_due = waiting;
                if let Some((fb, face)) = clock.as_mut() {
                    let date = file
                        .with_timer(|timer| Rx8025t::read(bus, timer))
                        .ok()
                        .filter(|s| s.time_usable())
                        .and_then(|s| s.datetime().ok());
                    face.draw(fb, date);
                    max_clock_us = max_clock_us.max(
                        file.with_timer(|timer| timer.now_us())
                            .wrapping_sub(waiting),
                    );
                    clock_updates += 1;
                }
            }
            if refill_count.is_multiple_of(320) {
                let _ = writeln!(
                    console,
                    "MEDIA PLAY PROGRESS frames={} max_refill_us={}",
                    total_frames, max_read_us
                );
            }
        }
        let _ = writeln!(
            console,
            "MEDIA PLAY CLOCK updates={} max_us={}",
            clock_updates, max_clock_us
        );
        // Descriptor EOF means drained AHB FIFO; allow the small I2S FIFO tail
        // to leave before muting (the next buffer contains silence near file EOF).
        file.with_timer(|timer| timer.delay_us(2_000));
        let _ = writeln!(
            console,
            "MEDIA PLAY SENT frames={} refills={} max_refill_us={} dma={:?}",
            total_frames,
            refill_count,
            max_read_us,
            tx.status()
        );
        Ok(())
    })();
    let mute = if let Some(codec) = codec.as_mut() {
        file.with_timer(|timer| codec.mute(bus, timer, true))
            .map_err(codec_error)
    } else {
        Ok(())
    };
    pa.set_low();
    let stop = file.with_timer(|timer| tx.stop(timer)).map_err(audio);
    let _ = writeln!(
        console,
        "MEDIA AUDIO STOP PA={} {:?}",
        pa.level(),
        tx.status()
    );
    result.and(mute).and(stop)
}

#[inline(never)]
fn memory_bench(
    internal: &esp32_p4::memory::Arena<'_, esp32_p4::memory::Internal>,
    external: &esp32_p4::memory::Arena<'_, esp32_p4::memory::External>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), hal::memory::Error> {
    use core::alloc::Layout;
    let mut fast = internal
        .allocate(Layout::from_size_align(128 * 1024, 64).unwrap())
        .map_err(|_| hal::memory::Error::CacheLayout(0))?;
    let mut large = external
        .allocate(Layout::from_size_align(1024 * 1024, 64).unwrap())
        .map_err(|_| hal::memory::Error::CacheLayout(0))?;
    for (name, slice) in [
        ("SRAM", fast.as_mut_slice()),
        ("PSRAM", large.as_mut_slice()),
    ] {
        let _ = writeln!(
            console,
            "MEMORY BENCH {} address={:08x} bytes={}",
            name,
            slice.as_ptr() as usize,
            slice.len()
        );
        unsafe {
            hal::memory::benchmark(slice.as_mut_ptr() as usize, slice.len(), timer, |b| {
                let bytes = (b.size * b.passes) as u64;
                let _ = writeln!(
                    console,
                    "MEMORY BENCH {} {:?} write_KiB_s={} read_KiB_s={} dependent_ns={}",
                    name,
                    b,
                    bytes * 1_000_000 / 1024 / b.write_us.max(1),
                    bytes * 1_000_000 / 1024 / b.read_us.max(1),
                    b.dependent_us * 1000 / b.dependent_steps as u64
                );
            })
        }?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct JpegTrial {
    decode_us: u64,
    total_us: u64,
    sd_us: u64,
    sd_calls: u32,
    sd_bytes: u64,
    pixels: u32,
    hash: u32,
    profile: esp32_p4::media::jpeg::Profile,
}

// A and B enter the same decoder instantiation and callback, selected by a
// runtime flag. Both consume/hash every RGB565 pixel. Timed code never logs.
#[inline(never)]
fn jpeg_trial(
    path: &str,
    fb: &mut esp32_p4::devices::framebuffer::Framebuffer,
    store_pixels: bool,
    profiling: bool,
    idct: esp32_p4::media::idct::Kind,
    timer: &mut hal::time::Timer,
    mut background: Option<&mut AudioWork>,
) -> Result<JpegTrial, esp32_p4::media::Error<FsProbeError>> {
    use esp32_p4::{
        media::{jpeg, Error},
        storage::{
            fat32::Fat32,
            sd::{Error as SdError, SdDevice},
            Partition,
        },
    };
    if let Some(audio) = background.as_mut() {
        audio.poll();
    }
    let store_pixels = core::hint::black_box(store_pixels);
    let started = timer.now_us();
    let transport = |e| {
        Error::Io(esp32_p4::storage::Error::Device(
            esp32_p4::storage::Error::Device(SdError::Card(e)),
        ))
    };
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(transport)?;
    card.configure_bus(timer).map_err(transport)?;
    let partition = Partition::mbr(SdDevice::new(card, timer), 0)
        .map_err(|e| Error::Io(esp32_p4::storage::Error::Device(e)))?;
    let mut fs = Fat32::mount(partition).map_err(Error::Io)?;
    let mut file = fs.open(path).map_err(Error::Io)?;
    let io_before = file.read_stats();
    let decode_started = file.with_timer(|t| t.now_us());
    let mut hash = 0x811c9dc5u32;
    let mut pixels_seen = 0u32;
    let mut profile = (if core::hint::black_box(profiling) {
        jpeg::Profile::new(hal::time::cycle_count)
    } else {
        jpeg::Profile::disabled()
    })
    .with_idct(core::hint::black_box(idct));
    jpeg::decode_profiled(
        &mut file,
        1024,
        600,
        |x, y, pixels| {
            for &pixel in pixels {
                hash = (hash ^ u32::from(pixel)).wrapping_mul(0x01000193);
            }
            pixels_seen += pixels.len() as u32;
            if store_pixels {
                fb.write_run(x, y, pixels);
            }
            if let Some(audio) = background.as_mut() {
                audio.poll();
            }
        },
        &mut profile,
    )?;
    fb.finish_writes();
    let ended = file.with_timer(|t| t.now_us());
    let io_after = file.read_stats();
    if pixels_seen != 1024 * 600 {
        return Err(Error::Invalid("JPEG benchmark pixel count"));
    }
    Ok(JpegTrial {
        decode_us: ended.wrapping_sub(decode_started) & ((1 << 48) - 1),
        total_us: ended.wrapping_sub(started) & ((1 << 48) - 1),
        sd_us: io_after.io_us - io_before.io_us,
        sd_calls: io_after.calls - io_before.calls,
        sd_bytes: io_after.bytes - io_before.bytes,
        pixels: pixels_seen,
        hash,
        profile,
    })
}

#[inline(never)]
fn jpeg_bench(
    path: &str,
    fb: &mut esp32_p4::devices::framebuffer::Framebuffer,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), esp32_p4::media::Error<FsProbeError>> {
    use esp32_p4::media::Error;
    let _ = writeln!(
        console,
        "JPEG BENCH START path={} order=ABBA A=u16+hash B=hash-only LCD=active caches={:x?}",
        path,
        hal::memory::cache_snapshot()
    );
    let mut reference = None;
    let mut times = [0u64; 4];
    for (pass, store) in [true, false, false, true].into_iter().enumerate() {
        let before = fb.checksum();
        let trial = jpeg_trial(
            path,
            fb,
            store,
            false,
            esp32_p4::media::idct::Kind::Matrix,
            timer,
            None,
        )?;
        let after = fb.checksum();
        times[pass] = trial.decode_us;
        let _=writeln!(console,"JPEG BENCH TRIAL pass={} mode={} decode_us={} total_us={} sd_us={} sd_calls={} sd_bytes={} pixels={} hash={:08x} fb_before={:08x} fb_after={:08x}",pass+1,if store {'A'}else{'B'},trial.decode_us,trial.total_us,trial.sd_us,trial.sd_calls,trial.sd_bytes,trial.pixels,trial.hash,before,after);
        if !fb.guards_ok() {
            return Err(Error::Invalid("JPEG benchmark framebuffer guard"));
        }
        if !store && before != after {
            return Err(Error::Invalid("JPEG checksum-only changed framebuffer"));
        }
        if reference.is_some_and(|value| value != (trial.hash, trial.pixels, trial.sd_bytes, after))
        {
            return Err(Error::Invalid("JPEG benchmark trials disagree"));
        }
        reference = Some((trial.hash, trial.pixels, trial.sd_bytes, after));
    }
    let a = (times[0] + times[3]) / 2;
    let b = (times[1] + times[2]) / 2;
    let delta = a as i64 - b as i64;
    let _ = writeln!(
        console,
        "JPEG BENCH RESULT A_mean_us={} B_mean_us={} delta_us={} delta_basis_points={} hash={:08x}",
        a,
        b,
        delta,
        delta * 10000 / a.max(1) as i64,
        reference.unwrap().0
    );
    Ok(())
}

#[inline(never)]
fn cycle_calibration(timer: &mut hal::time::Timer) -> (u64, u64) {
    let start = timer.now_us();
    let cycles = hal::time::cycle_count();
    timer.delay_us(20_000);
    let elapsed_cycles = hal::time::cycle_count().wrapping_sub(cycles);
    let elapsed_us = timer.now_us().wrapping_sub(start) & ((1 << 48) - 1);
    (elapsed_cycles, elapsed_us)
}

#[inline(never)]
fn jpeg_profile(
    path: &str,
    fb: &mut esp32_p4::devices::framebuffer::Framebuffer,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), esp32_p4::media::Error<FsProbeError>> {
    use esp32_p4::media::Error;
    let (cycles, us) = cycle_calibration(timer);
    if cycles == 0 {
        return Err(Error::Unsupported("machine cycle counter is stopped"));
    }
    let hz = cycles * 1_000_000 / us.max(1);
    let mut minimum_pair = u64::MAX;
    for _ in 0..64 {
        let start = hal::time::cycle_count();
        minimum_pair = minimum_pair.min(hal::time::cycle_count().wrapping_sub(start));
    }
    let _=writeln!(console,"JPEG PROFILE START path={} order=PUUP P=profile U=uninstrumented write=u16+hash clock_hz={} calibration_cycles={} calibration_us={} minimum_clock_pair_cycles={} caches={:x?}",path,hz,cycles,us,minimum_pair,hal::memory::cache_snapshot());
    let mut reference = None;
    let mut times = [0u64; 4];
    for (pass, enabled) in [true, false, false, true].into_iter().enumerate() {
        let trial = jpeg_trial(
            path,
            fb,
            true,
            enabled,
            esp32_p4::media::idct::Kind::Matrix,
            timer,
            None,
        )?;
        times[pass] = trial.decode_us;
        let framebuffer_hash = fb.checksum();
        let _=writeln!(console,"JPEG PROFILE TRIAL pass={} enabled={} decode_us={} total_us={} sd_us={} hash={:08x} pixels={} fb_hash={:08x}",pass+1,enabled,trial.decode_us,trial.total_us,trial.sd_us,trial.hash,trial.pixels,framebuffer_hash);
        if !fb.guards_ok()
            || reference.is_some_and(|r| r != (trial.hash, trial.pixels, framebuffer_hash))
        {
            return Err(Error::Invalid("JPEG profile trials disagree/guard"));
        }
        reference = Some((trial.hash, trial.pixels, framebuffer_hash));
        if enabled {
            if trial.profile.mcus != 2432
                || trial.profile.pixels != 614400
                || trial.profile.calls != [14592, 14592, 2432, 2432, 1]
            {
                return Err(Error::Invalid("JPEG profile stage counts"));
            }
            let names = [
                "entropy+dequant",
                "idct",
                "color",
                "output+checksum",
                "header",
            ];
            let mut accounted = 0u64;
            for (i, name) in names.into_iter().enumerate() {
                let ticks = trial.profile.ticks[i];
                let stage_us = ticks * 1_000_000 / hz;
                accounted += stage_us;
                let _=writeln!(console,"JPEG PROFILE STAGE pass={} stage={} cycles={} us={} calls={} cycles_per_call={}",pass+1,name,ticks,stage_us,trial.profile.calls[i],ticks/u64::from(trial.profile.calls[i]).max(1));
            }
            let _ = writeln!(
                console,
                "JPEG PROFILE ACCOUNTED pass={} stages_us={} residual_us={} mcus={} pixels={}",
                pass + 1,
                accounted,
                trial.decode_us as i64 - accounted as i64,
                trial.profile.mcus,
                trial.profile.pixels
            );
        }
    }
    let profiled = (times[0] + times[3]) / 2;
    let plain = (times[1] + times[2]) / 2;
    let (end_cycles, end_us) = cycle_calibration(timer);
    let end_hz = end_cycles * 1_000_000 / end_us.max(1);
    let _=writeln!(console,"JPEG PROFILE RESULT P_mean_us={} U_mean_us={} overhead_us={} overhead_basis_points={} clock_hz_before={} clock_hz_after={}",profiled,plain,profiled as i64-plain as i64,(profiled as i64-plain as i64)*10000/plain.max(1)as i64,hz,end_hz);
    Ok(())
}

// Read-only clock audit of the explicitly configured root/dividers.
#[inline(never)]
fn clock_audit(
    label: &str,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> u64 {
    let raw = |a: usize| unsafe { (a as *const u32).read_volatile() };
    let hp = raw(0x5011_1040);
    let div0 = raw(0x500e_6004);
    let div1 = raw(0x500e_6008);
    let div2 = raw(0x500e_600c);
    let (cycles, us) = cycle_calibration(timer);
    let _=writeln!(console,"CLOCK AUDIT {} hp_root={:08x} div0={:08x} div1={:08x} div2={:08x} source={} cpu_integer={} numerator={} denominator={} cycles={} us={} measured_hz={} cache_l1={:x} cache_l2={:x} l2_size={:x}",label,hp,div0,div1,div2,hp&3,((div0>>5)&255)+1,(div0>>13)&255,(div0>>21)&255,cycles,us,cycles*1_000_000/us.max(1),raw(0x3ff1_0008),raw(0x3ff1_0274),raw(0x3ff1_0278));
    cycles * 1_000_000 / us.max(1)
}

#[inline(never)]
fn jpeg_idct_bench(
    path: &str,
    fb: &mut esp32_p4::devices::framebuffer::Framebuffer,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), esp32_p4::media::Error<FsProbeError>> {
    use esp32_p4::media::{idct::Kind, Error};
    let (cycles, us) = cycle_calibration(timer);
    if cycles == 0 {
        return Err(Error::Unsupported("machine cycle counter stopped"));
    }
    let hz = cycles * 1_000_000 / us.max(1);
    let _ = writeln!(
        console,
        "JPEG IDCT START path={} order=LMML clock_hz={} LCD=active caches={:x?}",
        path,
        hz,
        hal::memory::cache_snapshot()
    );
    let mut references = [None; 2];
    let mut totals = [0u64; 2];
    let mut idct_totals = [0u64; 2];
    for (pass, kind) in [Kind::Loeffler, Kind::Matrix, Kind::Matrix, Kind::Loeffler]
        .into_iter()
        .enumerate()
    {
        hal::memory::instruction_counts_start();
        let trial_result = jpeg_trial(path, fb, true, true, kind, timer, None);
        let instruction_counts = hal::memory::instruction_counts_stop();
        let trial = trial_result?;
        let _ = writeln!(
            console,
            "JPEG ICACHE counts={:?} extra_d_l2i_l2d={:?}",
            instruction_counts,
            hal::memory::extra_cache_counts()
        );
        let slot = usize::from(kind == Kind::Loeffler);
        let framebuffer_hash = fb.checksum();
        let reference = (trial.hash, framebuffer_hash, trial.pixels, trial.sd_bytes);
        if !fb.guards_ok()
            || references[slot].is_some_and(|r| r != reference)
            || trial.profile.blocks != 14592
            || trial.profile.pixels != 614400
        {
            return Err(Error::Invalid("IDCT benchmark repeat/guard/count"));
        }
        references[slot] = Some(reference);
        let idct_us = trial.profile.ticks[1] * 1_000_000 / hz;
        totals[slot] += trial.decode_us;
        idct_totals[slot] += idct_us;
        let _=writeln!(console,"JPEG IDCT TRIAL pass={} kind={:?} decode_us={} total_us={} idct_us={} entropy_us={} color_us={} output_us={} sd_us={} blocks={} dc_only={} hash={:08x} fb_hash={:08x}",pass+1,kind,trial.decode_us,trial.total_us,idct_us,trial.profile.ticks[0]*1_000_000/hz,trial.profile.ticks[2]*1_000_000/hz,trial.profile.ticks[3]*1_000_000/hz,trial.sd_us,trial.profile.blocks,trial.profile.dc_only_blocks,trial.hash,framebuffer_hash);
    }
    let _=writeln!(console,"JPEG IDCT RESULT matrix_mean_us={} loeffler_mean_us={} matrix_idct_mean_us={} loeffler_idct_mean_us={} total_speedup_milli={} idct_speedup_milli={}",totals[0]/2,totals[1]/2,idct_totals[0]/2,idct_totals[1]/2,totals[0]*1000/totals[1].max(1),idct_totals[0]*1000/idct_totals[1].max(1));
    Ok(())
}

#[inline(never)]
fn sd_stress(
    arena: &esp32_p4::memory::Arena<'_, esp32_p4::memory::Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(u32, u32), &'static str> {
    use core::alloc::Layout;
    let mut reference = arena
        .allocate(Layout::from_size_align(3 * 8192, 64).unwrap())
        .map_err(|_| "alloc")?;
    let mut workspace = arena
        .allocate(Layout::from_size_align(8192 + 256, 64).unwrap())
        .map_err(|_| "alloc")?;
    let reference = reference.as_mut_slice();
    let workspace = workspace.as_mut_slice();
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(|_| "init")?;
    card.configure_bus(timer).map_err(|_| "bus")?;
    let lbas = [0, 2048, (card.info.sectors - 16) as u32];
    // Independent baseline: single-sector reads, normal (unpoisoned) path.
    for (bank, lba) in lbas.into_iter().enumerate() {
        for i in 0..16 {
            let sector = &mut reference[bank * 8192 + i * 512..bank * 8192 + (i + 1) * 512];
            card.read_sectors(lba + i as u32, sector, timer)
                .map_err(|_| "reference")?;
        }
    }
    let expect_dirty = hal::memory::cache_snapshot().l1_bypass & 16 == 0;
    let mut cases = 0u32;
    let mut dirty = 0u32;
    let start = timer.now_us();
    for round in 0..64u32 {
        for (bank, lba) in lbas.into_iter().enumerate() {
            for len in [512usize, 1024, 4096, 8192] {
                for offset in [0usize, 1, 3, 31, 63, 65] {
                    workspace.fill(0xa5);
                    let begin = 64 + offset;
                    let (tail, observed) = card
                        .read_poisoned(
                            lba,
                            &mut workspace[begin..begin + len],
                            0x9e3779b9u32.wrapping_mul(cases + 1),
                            timer,
                        )
                        .map_err(|_| "read")?;
                    if !tail
                        || workspace[..begin]
                            .iter()
                            .chain(workspace[begin + len..].iter())
                            .any(|b| *b != 0xa5)
                    {
                        return Err("guard");
                    }
                    if workspace[begin..begin + len] != reference[bank * 8192..bank * 8192 + len] {
                        return Err("bytes");
                    }
                    if expect_dirty && !observed {
                        return Err("not dirty");
                    }
                    cases += 1;
                    dirty += u32::from(observed);
                }
            }
        }
        if round % 16 == 15 {
            let _ = writeln!(
                console,
                "SD STRESS cases={} dirty={} us={}",
                cases,
                dirty,
                timer.now_us() - start
            );
        }
    }
    Ok((cases, dirty))
}

// Owns the AHB DMA channel and all producer state; no global callback pointers.
struct AudioWork {
    tx: hal::i2s::Tx,
    frames: [u32; 512],
    index: usize,
    refills: u32,
    last: u64,
    max_gap: u64,
    failed: bool,
}
impl AudioWork {
    fn poll(&mut self) {
        let now = hal::time::cycle_count();
        self.max_gap = self.max_gap.max(now.wrapping_sub(self.last));
        self.last = now;
        if self.failed {
            return;
        }
        for _ in 0..hal::i2s::STREAM_BUFFERS {
            match self.tx.stream_released(self.index) {
                Ok(false) => break,
                Ok(true) => {
                    let sample = ((self.refills % 127) + 1) * 32;
                    self.frames.fill(sample | (sample << 16));
                    if self.tx.stream_refill_now(self.index, &self.frames).is_err() {
                        self.failed = true;
                        break;
                    }
                    self.index = (self.index + 1) % hal::i2s::STREAM_BUFFERS;
                    self.refills += 1;
                }
                Err(_) => {
                    self.failed = true;
                    break;
                }
            }
        }
    }
}
#[inline(never)]
fn mixed_stress(
    fb: &mut esp32_p4::devices::framebuffer::Framebuffer,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    let mut pa = unsafe { hal::gpio::Output::<11>::new_low() };
    pa.set_low(); // Digital PCM transport test; speaker intentionally disabled.
    let mut audio = AudioWork {
        tx: unsafe { hal::i2s::Tx::new_stream(timer) }.map_err(|_| "init")?,
        frames: [0x01000100; 512],
        index: 0,
        refills: 0,
        last: hal::time::cycle_count(),
        max_gap: 0,
        failed: false,
    };
    audio.poll();
    if audio.failed || audio.tx.start(timer).is_err() {
        let _ = audio.tx.stop(timer);
        return Err("start");
    }
    let started = timer.now_us();
    let cycles = hal::time::cycle_count();
    audio.last = cycles;
    let result = (|| {
        for pass in 0..20 {
            let trial = jpeg_trial(
                "HOME.JPG",
                fb,
                true,
                false,
                esp32_p4::media::idct::Kind::Loeffler,
                timer,
                Some(&mut audio),
            )
            .map_err(|_| "jpeg")?;
            if audio.failed {
                return Err("audio");
            }
            if trial.hash != 0x534b3012 || !fb.guards_ok() {
                return Err("pixels");
            }
            if pass % 5 == 4 {
                let _ = writeln!(
                    console,
                    "MIXED frames={} refills={}",
                    pass + 1,
                    audio.refills
                );
            }
        }
        Ok(())
    })();
    let elapsed = timer.now_us() - started;
    let elapsed_cycles = hal::time::cycle_count() - cycles;
    let status = audio.tx.status();
    let stop = audio.tx.stop(timer).map_err(|_| "stop");
    let gap_us = audio.max_gap * elapsed / elapsed_cycles.max(1);
    let _ = writeln!(
        console,
        "MIXED PCM refills={} us={} max_gap_us={} status={:?}",
        audio.refills, elapsed, gap_us, status
    );
    result.and(stop)?;
    let expected_bck = elapsed * 512_000 / 1_000_000;
    let expected_blocks =
        (elapsed * 16_000 / 512 / 1_000_000) as u32 + hal::i2s::STREAM_BUFFERS as u32;
    if gap_us > 64_000
        || status.dma & 0x34 != 0
        || audio.refills.abs_diff(expected_blocks) > 2
        || u64::from(status.bck).abs_diff(expected_bck) > expected_bck / 100
    {
        return Err("timing");
    }
    Ok(())
}

fn ethernet_probe(
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), hal::ethernet::Error> {
    let mut mdio = unsafe { hal::ethernet::Mdio::new() };
    let _ = writeln!(console, "ETH MAC version={:08x}", mdio.version());
    let mut found = 0;
    for addr in 0..32 {
        let id1 = mdio.read(addr, 2, timer)?;
        let id2 = mdio.read(addr, 3, timer)?;
        if id1 == 0xffff || (id1 == 0 && id2 == 0) {
            continue;
        }
        found += 1;
        let control = mdio.read(addr, 0, timer)?;
        let _ = mdio.read(addr, 1, timer)?; // BMSR link is latch-low.
        let status = mdio.read(addr, 1, timer)?;
        let advertise = mdio.read(addr, 4, timer)?;
        let partner = mdio.read(addr, 5, timer)?;
        let _=writeln!(console,"ETH PHY addr={} ID={:04x}:{:04x} BMCR={:04x} BMSR={:04x} ANAR={:04x} ANLPAR={:04x} link={} autoneg_done={}",addr,id1,id2,control,status,advertise,partner,status&4!=0,status&32!=0);
    }
    let _ = writeln!(console, "ETH PHY found={}", found);
    Ok(())
}

fn ethernet_run(
    seconds: u32,
    arena: &esp32_p4::memory::Arena<'_, esp32_p4::memory::Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), hal::ethernet::Error> {
    use esp32_p4::net;
    let mut mdio = unsafe { hal::ethernet::Mdio::new() };
    let _ = mdio.read(1, 1, timer)?;
    let status = mdio.read(1, 1, timer)?;
    let common = mdio.read(1, 4, timer)? & mdio.read(1, 5, timer)?;
    if status & 0x24 != 0x24 || common & 0x100 == 0 {
        return Err(hal::ethernet::Error::Address);
    }
    let xid = timer.now_us() as u32;
    let mut emac = unsafe { hal::ethernet::Emac::start(arena, timer, net::MAC) }?;
    let _ = writeln!(
        console,
        "ETH START MAC={:02x?} status={:08x?} seconds={} ESC=stop",
        net::MAC,
        emac.status(),
        seconds
    );
    let mut rx = [0u8; net::FRAME];
    let mut tx = [0u8; net::FRAME];
    let mut offer = None;
    let mut lease: Option<net::Lease> = None;
    let mut received = 0u32;
    let mut sent = 0u32;
    let mut rx_errors = 0u32;
    let mut arp = 0u32;
    let mut echo = 0u32;
    let mut attempts = 0;
    let started = emac.now_us();
    let mut next = started;
    let mut expires = u64::MAX;
    while emac.now_us() - started < u64::from(seconds) * 1_000_000 {
        if console.read_byte() == Some(0x1b) {
            break;
        }
        let now = emac.now_us();
        if now >= expires {
            let _ = writeln!(
                console,
                "ETH DHCP lease expired; stop (renewal not implemented)"
            );
            break;
        }
        if lease.is_none() && now >= next && attempts < 10 {
            let n = net::dhcp(&mut tx, xid, offer);
            emac.transmit(&tx[..n])?;
            sent += 1;
            attempts += 1;
            next = now + 3_000_000;
            let _ = writeln!(
                console,
                "ETH DHCP {} attempt={}",
                if offer.is_some() {
                    "REQUEST"
                } else {
                    "DISCOVER"
                },
                attempts
            );
        }
        let n = match emac.receive(&mut rx) {
            Ok(Some(n)) => n,
            Ok(None) => continue,
            Err(hal::ethernet::Error::Frame(_)) => {
                rx_errors += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        received += 1;
        if received <= 10 {
            let _ = writeln!(
                console,
                "ETH RX n={} type={:02x}{:02x} src={:02x?}",
                n,
                rx[12],
                rx[13],
                &rx[6..12]
            );
        }
        if let Some((kind, reply)) = net::dhcp_reply(&rx[..n], xid) {
            if kind == 2
                && lease.is_none()
                && offer.is_none()
                && reply.address != [0; 4]
                && reply.server != [0; 4]
            {
                offer = Some(reply);
                next = now;
                attempts = 0;
                let _ = writeln!(console, "ETH DHCP OFFER {:?}", reply);
            } else if kind == 5
                && lease.is_none()
                && reply.seconds != 0
                && offer
                    .is_some_and(|old| old.address == reply.address && old.server == reply.server)
            {
                lease = Some(reply);
                expires = now + u64::from(reply.seconds) * 1_000_000;
                let a = reply.address;
                let _ = writeln!(
                    console,
                    "ETH DHCP BOUND IP={}.{}.{}.{} {:?}",
                    a[0], a[1], a[2], a[3], reply
                );
            } else if kind == 6
                && lease.is_none()
                && offer.is_some_and(|old| old.server == reply.server)
            {
                offer = None;
                next = now + 3_000_000;
            }
        }
        if let Some(lease) = lease {
            if let Some(n) = net::reply(&rx[..n], lease.address, &mut tx) {
                emac.transmit(&tx[..n])?;
                sent += 1;
                if tx[12..14] == [8, 6] {
                    arp += 1;
                } else {
                    echo += 1;
                }
            }
        }
    }
    let _=writeln!(console,"ETH SUMMARY rx={} tx={} rx_errors={} arp_replies={} echo_replies={} lease={:?} status={:08x?}",received,sent,rx_errors,arp,echo,lease,emac.status());
    drop(emac); // Confirm DMA stop before caller resets the arena.
    Ok(())
}

fn rtc_command(
    command: &str,
    bus: &mut hal::i2c::I2c,
    timer: &mut hal::time::Timer,
) -> Result<esp32_p4::devices::rx8025t::Snapshot, esp32_p4::devices::rx8025t::Error> {
    use esp32_p4::devices::rx8025t::{
        Alarm, AlarmDay, Compensation, DateTime, Error, Frequency, Settings, TimerClock,
    };
    let mut words = command.split_ascii_whitespace();
    let op = words.next().ok_or(Error::InvalidSetting)?;
    let number = |s: Option<&str>| -> Result<u16, Error> {
        s.and_then(|s| s.parse().ok()).ok_or(Error::InvalidSetting)
    };
    match op {
        "read" if words.next().is_none() => {}
        "init" | "set" => {
            let date = DateTime::parse(words.next().ok_or(Error::InvalidCalendar)?)
                .map_err(|_| Error::InvalidCalendar)?;
            if words.next().is_some() {
                return Err(Error::InvalidSetting);
            }
            if op == "init" {
                Rx8025t::initialize(bus, timer, date, Settings::default())?;
            } else {
                Rx8025t::set_datetime(bus, timer, date)?;
            }
        }
        "config" => {
            let compensation = match words.next() {
                Some("0.5") => Compensation::HalfSecond,
                Some("2") => Compensation::TwoSeconds,
                Some("10") => Compensation::TenSeconds,
                Some("30") => Compensation::ThirtySeconds,
                _ => return Err(Error::InvalidSetting),
            };
            let frequency = match words.next() {
                Some("32768") => Frequency::Hz32768,
                Some("1024") => Frequency::Hz1024,
                Some("1") => Frequency::Hz1,
                _ => return Err(Error::InvalidSetting),
            };
            let update_each_minute = match words.next() {
                Some("second") => false,
                Some("minute") => true,
                _ => return Err(Error::InvalidSetting),
            };
            let update_interrupt = match words.next() {
                Some("off") => false,
                Some("on") => true,
                _ => return Err(Error::InvalidSetting),
            };
            if words.next().is_some() {
                return Err(Error::InvalidSetting);
            }
            Rx8025t::configure(
                bus,
                timer,
                Settings {
                    compensation,
                    frequency,
                    update_each_minute,
                    update_interrupt,
                },
            )?;
        }
        "clear" if words.next().is_none() => Rx8025t::clear_flags(bus, timer)?,
        "ram" => {
            let value = number(words.next())?;
            if value > 255 || words.next().is_some() {
                return Err(Error::InvalidSetting);
            }
            Rx8025t::write_ram(bus, timer, value as u8)?;
        }
        "timer" => {
            let count = number(words.next())?;
            let clock = match words.next() {
                Some("4096") => TimerClock::Hz4096,
                Some("64") => TimerClock::Hz64,
                Some("second") => TimerClock::Second,
                Some("minute") => TimerClock::Minute,
                _ => return Err(Error::InvalidSetting),
            };
            if words.next().is_some() {
                return Err(Error::InvalidSetting);
            }
            Rx8025t::set_timer(
                bus,
                timer,
                if count == 0 { None } else { Some(count) },
                clock,
                false,
            )?;
        }
        "alarm-ack" if words.next().is_none() => Rx8025t::acknowledge_alarm(bus, timer)?,
        "alarm-off" if words.next().is_none() => Rx8025t::disable_alarm(bus, timer)?,
        "alarm" => {
            let hour = number(words.next())?;
            let minute = number(words.next())?;
            if hour > 23 || minute > 59 || words.next().is_some() {
                return Err(Error::InvalidSetting);
            }
            Rx8025t::set_alarm(
                bus,
                timer,
                Alarm {
                    hour: Some(hour as u8),
                    minute: Some(minute as u8),
                    day: AlarmDay::Any,
                    interrupt: true,
                },
            )?;
        }
        _ => return Err(Error::InvalidSetting),
    }
    Rx8025t::read(bus, timer)
}

/// Check fresh touch reports while audio DMA continues; acknowledge RTC before dismissal.
fn alarm_stop_touch(
    touch: &mut Gt911,
    bus: &mut hal::i2c::I2c,
    timer: &mut hal::time::Timer,
) -> bool {
    if let Ok(Some(frame)) = touch.read_frame(bus, timer) {
        if frame
            .points()
            .iter()
            .any(|p| esp32_p4::clock_face::stop_hit(p.x, p.y))
        {
            return Rx8025t::acknowledge_alarm(bus, timer).is_ok();
        }
    }
    false
}
