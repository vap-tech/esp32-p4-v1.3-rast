//! Bounded CPU/LCD contention probe; restores matrix settings and framebuffer.
use core::{alloc::Layout, fmt::Write};
use esp32_p4::{
    devices::{
        framebuffer::{self, Framebuffer},
        jd9165::Display,
    },
    hal,
    memory::{Arena, External, Internal},
};
const X: usize = 312;
const Y: usize = 480;
const W: usize = 400;
const H: usize = 88;
const BYTES: usize = W * H * 2;
const SCRATCH_BYTES: usize = framebuffer::WIDTH * H * 2 + X * 2;
pub struct Context<'a> {
    pub fb: &'a mut Framebuffer,
    pub panel: &'a mut Display,
    pub timer: &'a mut hal::time::Timer,
    pub console: &'a mut hal::usb_serial::Console,
    pub internal: &'a Arena<'a, Internal>,
    pub external: &'a Arena<'a, External>,
}
pub fn run(args: &str, context: Context<'_>) -> Result<(), &'static str> {
    let Context {
        fb,
        panel,
        timer,
        console,
        internal,
        external,
    } = context;
    let mut words = args.split_ascii_whitespace();
    let profile = words.next().ok_or("profile missing")?;
    let mode = words.next().ok_or("u16/u32/paced missing")?;
    let target = words.next().ok_or("frame/scratch missing")?;
    let rounds = words
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| (1..=200).contains(v))
        .ok_or("rounds 1..200")?;
    if words.next().is_some()
        || !matches!(
            mode,
            "u16" | "u32" | "paced" | "busy" | "fence-busy" | "probe"
        )
        || !matches!(target, "frame" | "scratch" | "linear")
    {
        return Err("invalid arguments");
    }
    let cache = hal::memory::cache_snapshot();
    if cache.l1_bypass & 16 == 0 || cache.l2_bypass & 32 == 0 {
        return Err("D-cache OFF, L2 bypass required");
    }
    let _ = writeln!(
        console,
        "ARBITRATION STAGE parsed stack={:?}",
        hal::memory::stack_usage()
    );
    let mut backup = internal
        .allocate(Layout::from_size_align(BYTES, 64).unwrap())
        .map_err(|_| "SRAM backup")?;
    let _ = writeln!(
        console,
        "ARBITRATION STAGE internal={:x} length={}",
        backup.as_mut_slice().as_mut_ptr() as usize,
        backup.len()
    );
    let mut scratch = external
        .allocate(Layout::from_size_align(SCRATCH_BYTES + 256, 64).unwrap())
        .map_err(|_| "PSRAM scratch")?;
    let scratch_base = scratch.as_mut_slice().as_mut_ptr() as usize;
    let _ = writeln!(console, "ARBITRATION STAGE external={:x}", scratch_base);
    // Preserve exact original pixels; isolated scratch is owned through its arena.
    for y in 0..H {
        for x in 0..W {
            let value = fb.pixel(X + x, Y + y).unwrap().to_le_bytes();
            backup.as_mut_slice()[(y * W + x) * 2..(y * W + x) * 2 + 2].copy_from_slice(&value);
        }
    }
    for off in [0, SCRATCH_BYTES + 128] {
        unsafe {
            ((scratch_base + off) as *mut u8).write_bytes(0xa5, 64);
        }
    }
    let _ = writeln!(console, "ARBITRATION STAGE backup done");
    let mut config = unsafe { hal::arbitration::Restore::save() };
    let _ = writeln!(console, "ARBITRATION STAGE registers saved");
    unsafe { config.apply(profile) }?;
    let _ = writeln!(console, "ARBITRATION STAGE profile applied");
    let _ = writeln!(
        console,
        "ARBITRATION START profile={} mode={} target={} rounds={} state={:x?}",
        profile,
        mode,
        target,
        rounds,
        hal::arbitration::snapshot()
    );
    timer.delay_us(40_000);
    panel.framebuffer_status(); // Discard setup/previous activity; each trial has its own latch.
    let mut failures = 0u32;
    let mut min_fifo = u32::MAX;
    let mut total = 0u64;
    let mut worst = 0u64;
    let mut rows = [0u16; W];
    let mut valid = true;
    let mut completed = 0;
    let probe_busy = matches!(mode, "busy" | "fence-busy" | "probe");
    let fence_busy = mode == "fence-busy";
    let wait_busy = mode != "probe";
    let mut busy_seen = 0u32;
    let mut polls = 0u32;
    let mut timeouts = 0u32;
    let mut max_wait_cycles = 0u64;
    'trials: for trial in 0..rounds {
        if console.read_byte() == Some(27) {
            break;
        }
        let color = if trial & 1 == 0 { 0x87e0 } else { 0x0841 };
        rows.fill(color);
        let at = timer.now_us();
        for y in 0..H {
            if mode == "paced" && target == "frame" {
                fb.write_run(X, Y + y, &rows);
            } else {
                let address = if target == "frame" {
                    framebuffer::CPU_ADDRESS + ((Y + y) * framebuffer::WIDTH + X) * 2
                } else {
                    scratch_base
                        + 128
                        + if target == "linear" {
                            (y * W + X) * 2
                        } else {
                            (y * framebuffer::WIDTH + X) * 2
                        }
                };
                if mode == "u16" {
                    for x in 0..W {
                        unsafe {
                            ((address + x * 2) as *mut u16).write_volatile(color);
                        }
                    }
                } else {
                    for x in 0..W / 2 {
                        unsafe {
                            ((address + x * 4) as *mut u32)
                                .write_volatile(u32::from(color) * 0x10001);
                        }
                        if probe_busy && (x + 1).is_multiple_of(16) {
                            if fence_busy {
                                unsafe {
                                    core::arch::asm!("fence rw, rw", options(nostack));
                                }
                            }
                            let start = hal::time::cycle_count();
                            let mut value = unsafe { (0x500e_5138 as *const u32).read_volatile() };
                            busy_seen += u32::from(value & 4 != 0);
                            if wait_busy {
                                while value & 4 != 0 {
                                    polls += 1;
                                    if hal::time::cycle_count().wrapping_sub(start) >= 160_000 {
                                        timeouts += 1;
                                        break 'trials;
                                    }
                                    value = unsafe { (0x500e_5138 as *const u32).read_volatile() };
                                }
                            }
                            max_wait_cycles =
                                max_wait_cycles.max(hal::time::cycle_count().wrapping_sub(start));
                        }
                        if mode == "paced" && (x + 1).is_multiple_of(16) {
                            unsafe {
                                core::arch::asm!("fence rw, rw", options(nostack));
                            }
                            let start = hal::time::cycle_count();
                            while hal::time::cycle_count().wrapping_sub(start) < 320 {
                                core::hint::spin_loop();
                            }
                        }
                    }
                }
            }
        }
        fb.finish_writes();
        let elapsed = timer.now_us().wrapping_sub(at);
        total += elapsed;
        worst = worst.max(elapsed);
        completed += 1;
        let raw = panel.framebuffer_status();
        min_fifo = min_fifo.min(raw[5] & 0x3fff);
        if raw[4] & 1 != 0 {
            failures += 1;
        }

        // Compare data before the next trial. Read-only traffic may also contend.
        for y in 0..H {
            for x in 0..W {
                let actual = if target == "frame" {
                    fb.pixel(X + x, Y + y).unwrap()
                } else {
                    unsafe {
                        ((scratch_base
                            + 128
                            + if target == "linear" {
                                (y * W + X + x) * 2
                            } else {
                                (y * framebuffer::WIDTH + X + x) * 2
                            }) as *const u16)
                            .read_volatile()
                    }
                };
                valid &= actual == color;
            }
        }
        timer.delay_us((trial * 7919) % 18000 + 1_000);
        let raw = panel.framebuffer_status();
        if raw[4] & 1 != 0 {
            failures += 1;
        }
        min_fifo = min_fifo.min(raw[5] & 0x3fff);
    }
    let _ = writeln!(
        console,
        "ARBITRATION BUSY seen={} extra_polls={} timeouts={} max_wait_cycles={}",
        busy_seen, polls, timeouts, max_wait_cycles
    );
    // Restore policy before restoring pixels (the normal paced path).
    drop(config);
    for y in 0..H {
        for (x, pixel) in rows.iter_mut().enumerate() {
            let i = (y * W + x) * 2;
            let b = backup.as_slice();
            *pixel = u16::from_le_bytes([b[i], b[i + 1]]);
        }
        fb.write_run(X, Y + y, &rows);
    }
    fb.finish_writes();
    let guards = [0, SCRATCH_BYTES + 128]
        .into_iter()
        .all(|off| scratch.as_slice()[off..off + 64].iter().all(|b| *b == 0xa5))
        && fb.guards_ok();
    let _=writeln!(console,"ARBITRATION RESULT profile={} mode={} target={} rounds={} underrun_samples={} min_fifo={} write_total_us={} write_max_us={} data_ok={} guards={} restored={:x?}",profile,mode,target,completed,failures,min_fifo,total,worst,valid,guards,hal::arbitration::snapshot());
    if timeouts != 0 {
        return Err("H2X BUSY timeout");
    }
    if !guards || !valid {
        return Err("data/guard mismatch");
    }
    Ok(())
}
