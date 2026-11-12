//! Cooperative microphone -> AHB RX ring -> FAT32 streaming WAV. PA stays off.
use core::{alloc::Layout, fmt::Write};
use esp32_p4::{
    clock_face::{self, ClockFace},
    devices::{es8311::Es8311, framebuffer::Framebuffer, gt911::Gt911, rx8025t::Rx8025t},
    hal,
    media::wav,
    memory::{Arena, Internal},
    storage::{fat32::Fat32, sd::SdDevice, Partition},
};
use hal::i2s::{
    capture::{Ring, BLOCKS, WORDS},
    Tx,
};
const DATA_BYTES: usize = BLOCKS * WORDS * 4;
const DESC_OFFSET: usize = DATA_BYTES + 64;
const TOTAL: usize = DESC_OFFSET + BLOCKS * 64 + 64;
pub struct Context<'a> {
    pub bus: &'a mut hal::i2c::I2c,
    pub timer: &'a mut hal::time::Timer,
    pub console: &'a mut hal::usb_serial::Console,
    pub arena: &'a Arena<'a, Internal>,
    pub panel: Option<&'a mut esp32_p4::devices::jd9165::Display>,
    pub touch: &'a mut Gt911,
    pub ui: Option<(&'a mut Framebuffer, &'a mut ClockFace)>,
}
#[derive(Debug)]
pub struct Report {
    pub frames: u32,
    pub elapsed_us: u64,
    pub peak: u16,
    pub reference_peak: u16,
    pub mean_abs: u32,
    pub fnv: u32,
    pub max_write_us: u64,
    pub max_clock_us: u64,
    pub stopped: bool,
    pub lcd_write: u32,
    pub lcd_ui: u32,
}
pub fn run(path: &str, seconds: u32, context: Context<'_>) -> Result<Report, &'static str> {
    let Context {
        bus,
        timer,
        console,
        arena,
        touch,
        mut panel,
        mut ui,
    } = context;
    let baseline = take_lcd(&mut panel);
    let _ = writeln!(console, "RECORD LCD baseline={}", baseline);
    if !(1..=600).contains(&seconds) {
        return Err("duration 1..600 seconds");
    }
    let cache = hal::memory::cache_snapshot();
    if cache.l1_bypass & 16 == 0 || cache.l2_bypass & 32 == 0 {
        return Err("requires D-cache OFF/L2 bypass");
    }
    let mut allocation = arena
        .allocate(Layout::from_size_align(TOTAL, 64).unwrap())
        .map_err(|_| "RX arena")?;
    let base = allocation.as_mut_slice().as_mut_ptr() as usize;
    for offset in [DATA_BYTES, DESC_OFFSET + BLOCKS * 64] {
        unsafe {
            ((base + offset) as *mut u8).write_bytes(0xa5, 64);
        }
    }
    unsafe { hal::i2s::capture::publish_region(base, TOTAL, timer) }.map_err(|_| "RX publish")?;
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(|_| "SD init")?;
    card.configure_bus(timer).map_err(|_| "SD bus")?;
    let partition = Partition::mbr(SdDevice::new(card, timer), 0).map_err(|_| "SD partition")?;
    let mut fs = Fat32::mount(partition).map_err(|_| "FAT mount")?;
    let mut writer = fs
        .create_stream(path)
        .map_err(|_| "create-new (exists/dirty/full/name)")?;
    if writer.write(&wav::recording_header(0).unwrap()).is_err() {
        return Err("WAV header write; volume dirty");
    }
    let _ = writeln!(console, "RECORD LCD storage-setup={}", take_lcd(&mut panel));
    let mut pa = unsafe { hal::gpio::Output::<11>::new_low() };
    pa.set_low();
    let mut tx = None;
    let mut rx = None;
    let mut codec = None;
    if let Some((fb, _)) = ui.as_mut() {
        clock_face::alarm_button(fb, true);
        clock_face::record_meter(fb, 0, 0, true);
    }
    let _ = writeln!(console, "RECORD LCD button-draw={}", take_lcd(&mut panel));
    let result = (|| {
        tx = Some(
            writer
                .with_timer(|t| unsafe { Tx::new(&[0], t) })
                .map_err(|_| "I2S TX init")?,
        );
        writer
            .with_timer(|t| tx.as_mut().unwrap().start(t))
            .map_err(|_| "I2S clock start")?;
        codec = Some(
            writer
                .with_timer(|t| Es8311::microphone(bus, t))
                .map_err(|_| "microphone codec init")?,
        );
        rx = Some(
            writer
                .with_timer(|t| unsafe { Ring::start_pin(base, base + DESC_OFFSET, 48, t) })
                .map_err(|_| "RX start")?,
        );
        let started = writer.with_timer(|t| t.now_us());
        let mut last_ready = started;
        let mut touch_at = started;
        let mut ui_at = started;
        let mut report = Report {
            frames: 0,
            elapsed_us: 0,
            peak: 0,
            reference_peak: 0,
            mean_abs: 0,
            fnv: 2166136261,
            max_write_us: 0,
            max_clock_us: 0,
            stopped: false,
            lcd_write: 0,
            lcd_ui: 0,
        };
        let mut sum_abs = 0u64;
        let mut interval_peak = 0u16;
        let mut first = true;
        let mut pcm = [0u8; WORDS * 2];
        let _ = writeln!(
            console,
            "RECORD START path={} PCM16 mono 16000Hz gain=24dB duration={} STOP/ESC",
            path, seconds
        );
        loop {
            if console.read_byte() == Some(27) {
                report.stopped = true;
                break;
            }
            let now = writer.with_timer(|t| t.now_us());
            if rx
                .as_ref()
                .unwrap()
                .ready()
                .map_err(|_| "RX overrun/descriptor")?
            {
                last_ready = now;
                let skip = usize::from(first);
                first = false;
                let n = (WORDS - skip).min((seconds * 16000 - report.frames) as usize);
                for i in 0..n {
                    let word = unsafe { rx.as_ref().unwrap().word(i + skip) };
                    let sample = word as i16;
                    let value = sample.unsigned_abs();
                    report.peak = report.peak.max(value);
                    interval_peak = interval_peak.max(value);
                    report.reference_peak = report
                        .reference_peak
                        .max(((word >> 16) as i16).unsigned_abs());
                    sum_abs += u64::from(value);
                    let b = sample.to_le_bytes();
                    pcm[i * 2..i * 2 + 2].copy_from_slice(&b);
                    for v in b {
                        report.fnv = (report.fnv ^ u32::from(v)).wrapping_mul(16777619);
                    }
                }
                unsafe {
                    rx.as_mut().unwrap().release();
                }
                let write_at = writer.with_timer(|t| t.now_us());
                writer
                    .write(&pcm[..n * 2])
                    .map_err(|_| "SD PCM write; volume dirty")?;
                report.max_write_us = report
                    .max_write_us
                    .max(writer.with_timer(|t| t.now_us()).wrapping_sub(write_at));
                report.lcd_write += u32::from(take_lcd(&mut panel));
                report.frames += n as u32;
                if report.frames >= seconds * 16000 {
                    break;
                }
                continue; // Drain RX first, then service UI in slack time.
            }
            if now.wrapping_sub(last_ready) > 200_000 {
                return Err("RX timeout");
            }
            if now.wrapping_sub(touch_at) >= 20_000 {
                touch_at = now;
                if let Ok(Some(frame)) = writer.with_timer(|t| touch.read_frame(bus, t)) {
                    if frame
                        .points()
                        .iter()
                        .any(|p| clock_face::stop_hit(p.x, p.y))
                    {
                        report.stopped = true;
                        break;
                    }
                }
            }
            if now.wrapping_sub(ui_at) >= 1_000_000 {
                ui_at = now;
                if let Some((fb, face)) = ui.as_mut() {
                    let date = writer
                        .with_timer(|t| Rx8025t::read(bus, t))
                        .ok()
                        .filter(|s| s.time_usable())
                        .and_then(|s| s.datetime().ok());
                    face.draw(fb, date);
                    clock_face::record_meter(fb, report.frames / 16000, interval_peak, true);
                }
                report.lcd_ui += u32::from(take_lcd(&mut panel));
                report.max_clock_us = report
                    .max_clock_us
                    .max(writer.with_timer(|t| t.now_us()).wrapping_sub(now));
                let _ = writeln!(
                    console,
                    "RECORD LEVEL seconds={} peak={} reference={} frames={}",
                    report.frames / 16000,
                    interval_peak,
                    report.reference_peak,
                    report.frames
                );
                interval_peak = 0;
            }
        }
        report.elapsed_us = writer.with_timer(|t| t.now_us()).wrapping_sub(started);
        report.mean_abs = (sum_abs / u64::from(report.frames.max(1))) as u32;
        Ok(report)
    })();
    // Every return path parks RX before arena allocations can be reclaimed.
    let rx_stop = rx
        .as_mut()
        .map(|r| writer.with_timer(|t| r.stop(t)))
        .transpose();
    let mute = codec
        .as_mut()
        .map(|c| writer.with_timer(|t| c.mute(bus, t, true)))
        .transpose();
    pa.set_low();
    let tx_stop = tx
        .as_mut()
        .map(|t| writer.with_timer(|timer| t.stop(timer)))
        .transpose();
    if let Some((fb, _)) = ui.as_mut() {
        clock_face::record_meter(fb, 0, 0, false);
        clock_face::alarm_button(fb, false);
    }
    let _ = writeln!(console, "RECORD LCD cleanup={}", take_lcd(&mut panel));
    let guards = [DATA_BYTES, DESC_OFFSET + BLOCKS * 64]
        .into_iter()
        .all(|off| {
            (0..64).all(|i| unsafe {
                ((base + off + 0x4000_0000 + i) as *const u8).read_volatile() == 0xa5
            })
        });
    let _ = writeln!(
        console,
        "RECORD STOP RX={:?} TX={:?} mute={:?} guards={}",
        rx_stop, tx_stop, mute, guards
    );
    if tx_stop.is_err() || rx_stop.is_err() {
        panic!("audio DMA stop not confirmed");
    }
    let report = match result {
        Ok(r) if guards && mute.is_ok() => r,
        other => {
            let aborted = writer.abort();
            let _ = writeln!(console, "RECORD ABORT {:?}", aborted);
            return Err(other.err().unwrap_or("audio cleanup/guard"));
        }
    };
    if report.frames == 0 {
        writer.abort().map_err(|_| "empty abort")?;
        return Err("no PCM samples");
    }
    writer
        .rewrite_prefix(&wav::recording_header(report.frames * 2).unwrap())
        .map_err(|_| "final WAV header; volume dirty")?;
    writer.finish().map_err(|_| "WAV finish; volume dirty")?;
    let mut file = fs.open(path).map_err(|_| "verify reopen")?;
    let wave = wav::Wave::parse(&mut file).map_err(|_| "verify WAV header")?;
    if wave.data_bytes != report.frames * 2 || wave.data_offset != 512 {
        return Err("verify sizes");
    }
    let mut verify = [0u8; 1024];
    let mut left = wave.data_bytes;
    let mut hash = 2166136261u32;
    while left != 0 {
        let n = (left as usize).min(verify.len());
        let read = file.read(&mut verify[..n]).map_err(|_| "verify PCM read")?;
        if read != n {
            return Err("verify short PCM");
        }
        for byte in &verify[..n] {
            hash = (hash ^ u32::from(*byte)).wrapping_mul(16777619);
        }
        left -= n as u32;
    }
    if hash != report.fnv {
        return Err("verify PCM checksum");
    }
    let _ = writeln!(
        console,
        "RECORD VERIFIED bytes={} FNV={:08x}",
        wave.data_bytes, hash
    );
    Ok(report)
}

fn take_lcd(panel: &mut Option<&mut esp32_p4::devices::jd9165::Display>) -> bool {
    panel
        .as_mut()
        .is_some_and(|panel| panel.framebuffer_status()[4] & 1 != 0)
}
