//! Explicit offscreen hardware JPEG tests. The displayed framebuffer is untouched.
use core::{alloc::Layout, fmt::Write};
use esp32_p4::{
    hal,
    memory::{Arena, External, Internal},
};
fn one(
    bytes: &[u8],
    large: &Arena<'_, External>,
    dma: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    name: &str,
    dump: bool,
) -> Result<(), &'static str> {
    let h = hal::jpeg::Header::parse(bytes).map_err(|_| "header")?;
    let length = h.output_bytes();
    let mut source = large
        .allocate_paced(
            Layout::from_size_align(bytes.len().next_multiple_of(64) + 128, 64).unwrap(),
        )
        .map_err(|_| "source memory")?;
    let mut output = large
        .allocate_paced(Layout::from_size_align(length.next_multiple_of(64) + 128, 64).unwrap())
        .map_err(|_| "output memory")?;
    {
        let b = source.as_mut_slice();
        b[..64].fill(0xa5);
        let end = b.len() - 64;
        b[end..].fill(0xa5);
        esp32_p4::web::upload::copy_paced(&mut b[64..64 + bytes.len()], bytes);
    }
    {
        let b = output.as_mut_slice();
        b[..64].fill(0x5a);
        let end = b.len() - 64;
        b[end..].fill(0x5a);
    }
    if name == "early-eoi" {
        let sos = h.sos;
        let len = u16::from_be_bytes([bytes[sos + 2], bytes[sos + 3]]) as usize;
        let at = 64 + sos + 2 + len;
        source.as_mut_slice()[at..at + 2].copy_from_slice(&[255, 217]);
    }
    let mut descriptors = dma
        .allocate(Layout::from_size_align(256, 64).unwrap())
        .map_err(|_| "descriptors")?;
    descriptors.as_mut_slice()[..64].fill(0x3c);
    descriptors.as_mut_slice()[192..].fill(0x3c);
    let mut sram_source = if name.starts_with("sram") {
        Some(
            dma.allocate(Layout::from_size_align(source.len(), 64).unwrap())
                .map_err(|_| "sram source")?,
        )
    } else {
        None
    };
    let mut sram_output = if name == "sram-both" {
        Some(
            dma.allocate(Layout::from_size_align(output.len(), 64).unwrap())
                .map_err(|_| "sram output")?,
        )
    } else {
        None
    };
    if let Some(b) = sram_source.as_mut() {
        b.as_mut_slice().copy_from_slice(source.as_slice());
    }
    if let Some(b) = sram_output.as_mut() {
        b.as_mut_slice().copy_from_slice(output.as_slice());
    }
    let input_slice = sram_source
        .as_ref()
        .map_or(source.as_slice(), |b| b.as_slice());
    let output_slice = sram_output
        .as_mut()
        .map_or(output.as_mut_slice(), |b| b.as_mut_slice());
    let start = timer.now_us();
    let result = unsafe {
        hal::jpeg::decode(
            &input_slice[64..64 + bytes.len()],
            &mut output_slice[64..64 + length],
            &h,
            &mut descriptors.as_mut_slice()[64..192],
            hal::jpeg::ColorConversion::JpegFull,
            timer,
        )
    };
    let us = timer.now_us() - start;
    if let Some(b) = sram_output.as_ref() {
        output.as_mut_slice().copy_from_slice(b.as_slice());
    }
    if let Some(b) = sram_source.as_ref() {
        if b.as_slice() != source.as_slice() {
            return Err("SRAM source modified");
        }
    }
    let _ = writeln!(
        console,
        "JPEG HW name={} {}x{} stride={} rows={} components={} output={} us={} result={:x?}",
        name, h.width, h.height, h.stride, h.rows, h.components, length, us, result
    );
    for (b, guard) in [(&source, 0xa5), (&output, 0x5a)] {
        let s = b.as_slice();
        if !s[..64]
            .iter()
            .chain(s[s.len() - 64..].iter())
            .all(|v| *v == guard)
        {
            return Err("DMA guard damaged");
        }
    }
    if !descriptors.as_slice()[..64]
        .iter()
        .chain(descriptors.as_slice()[192..].iter())
        .all(|v| *v == 0x3c)
    {
        return Err("descriptor guard damaged");
    }
    result.map_err(|_| "hardware decode")?;
    let pixels = &output.as_slice()[64..64 + length];
    let mut hash = 0x811c9dc5u32;
    for b in pixels {
        hash = (hash ^ u32::from(*b)).wrapping_mul(16777619);
    }
    let _ = writeln!(
        console,
        "JPEG HW hash={:08x} guards=true samples={:02x?}",
        hash,
        &pixels[..pixels.len().min(32)]
    );
    if h.components == 1 && name == "gray16" && pixels.iter().any(|v| *v != 128 && *v != 127) {
        return Err("gray reference mismatch");
    }
    if matches!(
        name,
        "rgb444" | "rgb422" | "rgb420" | "sram-source" | "sram-both"
    ) {
        for (i, word) in pixels.as_chunks::<2>().0.iter().enumerate() {
            let actual = u16::from_le_bytes(*word);
            let band = (i % 32) / 8;
            let expected = [0xf800, 0x07e0, 0x001f, 0x7bef][band];
            // Neutral JPEG rounding can cross one RGB565 quantization step
            // independently in each channel; primary-color bands remain exact.
            let gray_close = band == 3
                && (actual >> 11).abs_diff(expected >> 11) <= 1
                && ((actual >> 5) & 63).abs_diff((expected >> 5) & 63) <= 1
                && (actual & 31).abs_diff(expected & 31) <= 1;
            if actual != expected && !gray_close {
                let _ = writeln!(
                    console,
                    "JPEG HW mismatch pixel={} actual={:04x} expected={:04x}",
                    i, actual, expected
                );
                return Err("RGB chart/reorder/byte order mismatch");
            }
        }
    }
    if dump {
        let mut text = [b' '; 74];
        text[..9].copy_from_slice(b"JPEG OUT ");
        for line in pixels.chunks(32) {
            for (i, &v) in line.iter().enumerate() {
                text[9 + i * 2] = b"0123456789abcdef"[(v >> 4) as usize];
                text[10 + i * 2] = b"0123456789abcdef"[(v & 15) as usize];
            }
            text[9 + line.len() * 2] = b'\n';
            let _ = console.write_str(core::str::from_utf8(&text[..10 + line.len() * 2]).unwrap());
        }
    }
    Ok(())
}
pub fn run(
    path: &str,
    large: &Arena<'_, External>,
    work: &Arena<'_, Internal>,
    dma: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    let (path, dump) = path
        .strip_prefix("dump ")
        .map_or((path, false), |p| (p, true));
    let result = if path == "stress" {
        for _ in 0..100 {
            for (name, bytes) in [
                (
                    "gray16",
                    include_bytes!("../tests/fixtures/hw-gray16.jpg").as_slice(),
                ),
                (
                    "rgb444",
                    include_bytes!("../tests/fixtures/hw-color-0.jpg").as_slice(),
                ),
                (
                    "rgb422",
                    include_bytes!("../tests/fixtures/hw-color-1.jpg").as_slice(),
                ),
                (
                    "rgb420",
                    include_bytes!("../tests/fixtures/hw-color-2.jpg").as_slice(),
                ),
                (
                    "sram-both",
                    include_bytes!("../tests/fixtures/hw-color-2.jpg").as_slice(),
                ),
            ] {
                one(bytes, large, dma, timer, console, name, false)?;
                assert!(large.reset_if_unused());
                assert!(dma.reset_if_unused());
            }
        }
        Ok(())
    } else if path == "test" {
        let mut outcome = Ok(());
        for (name, bytes) in [
            (
                "gray16",
                include_bytes!("../tests/fixtures/hw-gray16.jpg").as_slice(),
            ),
            (
                "rgb444",
                include_bytes!("../tests/fixtures/hw-color-0.jpg").as_slice(),
            ),
            (
                "rgb422",
                include_bytes!("../tests/fixtures/hw-color-1.jpg").as_slice(),
            ),
            (
                "rgb420",
                include_bytes!("../tests/fixtures/hw-color-2.jpg").as_slice(),
            ),
        ] {
            let r = one(bytes, large, dma, timer, console, name, false);
            if r.is_err() {
                outcome = r;
                break;
            }
            assert!(large.reset_if_unused());
            assert!(dma.reset_if_unused());
        }
        if outcome.is_ok() {
            for name in ["sram-source", "sram-both"] {
                outcome = one(
                    include_bytes!("../tests/fixtures/hw-color-2.jpg"),
                    large,
                    dma,
                    timer,
                    console,
                    name,
                    false,
                );
                if outcome.is_err() {
                    break;
                }
                assert!(large.reset_if_unused());
                assert!(dma.reset_if_unused());
            }
        }
        if outcome.is_ok() {
            let bad = one(
                include_bytes!("../tests/fixtures/hw-color-2.jpg"),
                large,
                dma,
                timer,
                console,
                "early-eoi",
                false,
            );
            let _ = writeln!(console, "JPEG HW NEGATIVE early-eoi result={:?}", bad);
            if bad != Err("hardware decode") {
                outcome = Err("negative test expected hardware error");
            }
            assert!(large.reset_if_unused());
            assert!(dma.reset_if_unused());
            if outcome.is_ok() {
                outcome = one(
                    include_bytes!("../tests/fixtures/hw-color-2.jpg"),
                    large,
                    dma,
                    timer,
                    console,
                    "rgb420",
                    false,
                );
            }
        }
        outcome
    } else {
        let (bytes, len) = load(path, large, work, timer)?;
        one(
            &bytes.as_slice()[..len],
            large,
            dma,
            timer,
            console,
            path,
            dump,
        )
    };
    assert!(large.reset_if_unused());
    assert!(work.reset_if_unused());
    assert!(dma.reset_if_unused());
    result
}

#[inline(never)]
fn load<'a>(
    path: &str,
    large: &'a Arena<'_, External>,
    work: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
) -> Result<(esp32_p4::memory::Buffer<'a, External>, usize), &'static str> {
    // Use SRAM for SD RX and pace CPU stores to PSRAM, preserving LCD access.
    let mut scratch = work
        .allocate(Layout::from_size_align(8192, 64).unwrap())
        .map_err(|_| "read scratch")?;
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(|_| "SD init")?;
    card.configure_bus(timer).map_err(|_| "SD bus")?;
    let part =
        esp32_p4::storage::Partition::mbr(esp32_p4::storage::sd::SdDevice::new(card, timer), 0)
            .map_err(|_| "partition")?;
    let mut fs = esp32_p4::storage::fat32::Fat32::mount(part).map_err(|_| "FAT mount")?;
    let mut file = fs.open(path).map_err(|_| "open JPEG")?;
    let len = file.len() as usize;
    if len == 0 || len > 8 * 1024 * 1024 {
        return Err("JPEG size");
    }
    let mut bytes = large
        .allocate_paced(Layout::from_size_align(len.next_multiple_of(64), 64).unwrap())
        .map_err(|_| "input memory")?;
    let mut at = 0;
    while at < len {
        let n = file
            .read(&mut scratch.as_mut_slice()[..(len - at).min(8192)])
            .map_err(|_| "SD read")?;
        if n == 0 {
            return Err("short JPEG");
        }
        esp32_p4::web::upload::copy_paced(
            &mut bytes.as_mut_slice()[at..at + n],
            &scratch.as_slice()[..n],
        );
        at += n;
    }
    Ok((bytes, len))
}
