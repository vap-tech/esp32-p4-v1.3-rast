//! Offscreen PPA fill validation; never publishes a new LCD framebuffer.
use core::{alloc::Layout, fmt::Write};
use esp32_p4::{
    hal,
    memory::{Arena, External, Internal},
};
pub fn run(
    large: &Arena<'_, External>,
    dma: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    for (width, height, rect, color) in [
        (
            2u16,
            1u16,
            hal::ppa::Rect {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
            },
            0xffffu16,
        ),
        (
            64,
            32,
            hal::ppa::Rect {
                x: 62,
                y: 31,
                width: 2,
                height: 1,
            },
            0xb5a7,
        ),
        (
            64u16,
            32u16,
            hal::ppa::Rect {
                x: 4,
                y: 3,
                width: 20,
                height: 17,
            },
            0xf800u16,
        ),
        (
            1024,
            600,
            hal::ppa::Rect {
                x: 0,
                y: 0,
                width: 1024,
                height: 600,
            },
            0x07e0,
        ),
        (
            64,
            32,
            hal::ppa::Rect {
                x: 0,
                y: 0,
                width: 64,
                height: 32,
            },
            0x001f,
        ),
    ] {
        let length = usize::from(width) * usize::from(height) * 2;
        let mut output = large
            .allocate_paced(Layout::from_size_align(length + 128, 64).unwrap())
            .map_err(|_| "output memory")?;
        // Full canaries also form the untouched-pixel reference for the partial fill.
        {
            let b = output.as_mut_slice();
            for chunk in b.chunks_mut(64) {
                esp32_p4::web::upload::copy_paced(chunk, &[0x5a; 64][..chunk.len()]);
            }
        }
        let mut desc = dma
            .allocate(Layout::from_size_align(192, 64).unwrap())
            .map_err(|_| "descriptor memory")?;
        desc.as_mut_slice().fill(0x3c);
        let begin = timer.now_us();
        let result = unsafe {
            hal::ppa::fill_rgb565(
                &mut output.as_mut_slice()[64..64 + length],
                width,
                height,
                rect,
                color,
                &mut desc.as_mut_slice()[64..128],
                timer,
            )
        };
        let us = timer.now_us() - begin;
        let _ = writeln!(
            console,
            "PPA FILL {}x{} rect={:?} color={:04x} us={} result={:x?}",
            width, height, rect, color, us, result
        );
        let b = output.as_slice();
        if !b[..64]
            .iter()
            .chain(b[b.len() - 64..].iter())
            .all(|v| *v == 0x5a)
        {
            return Err("output guard");
        }
        if !desc.as_slice()[..64]
            .iter()
            .chain(desc.as_slice()[128..].iter())
            .all(|v| *v == 0x3c)
        {
            return Err("descriptor guard");
        }
        result.map_err(|_| "fill hardware")?;
        for (i, word) in b[64..64 + length].as_chunks::<2>().0.iter().enumerate() {
            let x = (i % usize::from(width)) as u16;
            let y = (i / usize::from(width)) as u16;
            let inside =
                x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
            let expected = if inside { color } else { 0x5a5a };
            if u16::from_le_bytes(*word) != expected {
                let _ = writeln!(
                    console,
                    "PPA mismatch x={} y={} got={:04x} expected={:04x}",
                    x,
                    y,
                    u16::from_le_bytes(*word),
                    expected
                );
                return Err("pixel reference");
            }
        }
        let _ = writeln!(console, "PPA FILL pixels=true guards=true");
        drop(desc);
        drop(output);
        assert!(large.reset_if_unused());
        assert!(dma.reset_if_unused());
    }
    Ok(())
}

fn pattern(x: u16, y: u16) -> u16 {
    ((x & 31) << 11) | ((y & 63) << 5) | ((x + y) & 31)
}
/// No scaling: every output pixel has a unique, independently computed source.
pub fn srm(
    large: &Arena<'_, External>,
    dma: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    use hal::ppa::{Rect, Rotation, Srm, Transform};
    for rotation in [
        Rotation::Deg0,
        Rotation::Deg90,
        Rotation::Deg180,
        Rotation::Deg270,
    ] {
        for (mx, my) in [(false, false), (true, false), (false, true), (true, true)] {
            let cfg = Srm {
                source_size: (32, 24),
                source: Rect {
                    x: 4,
                    y: 2,
                    width: 20,
                    height: 12,
                },
                destination_size: (64, 48),
                destination: (8, 5),
                transform: Transform {
                    rotation,
                    mirror_x: mx,
                    mirror_y: my,
                    ..Transform::default()
                },
            };
            srm_one(large, dma, timer, console, cfg)?;
            assert!(large.reset_if_unused());
            assert!(dma.reset_if_unused());
        }
    }
    for (width, height, sx, sy, rotation, mx, my) in [
        (20, 12, 32, 32, Rotation::Deg0, false, false),
        (20, 12, 8, 8, Rotation::Deg0, false, false),
        (32, 24, 4, 4, Rotation::Deg0, false, false),
        (14, 18, 19, 13, Rotation::Deg0, false, false),
        (20, 12, 32, 8, Rotation::Deg0, false, false),
        (32, 32, 1, 1, Rotation::Deg0, false, false),
        (20, 12, 32, 32, Rotation::Deg90, true, false),
        (20, 12, 8, 8, Rotation::Deg270, false, true),
    ] {
        let cfg = Srm {
            source_size: (40, 40),
            source: Rect {
                x: 4,
                y: 2,
                width,
                height,
            },
            destination_size: (64, 64),
            destination: (8, 5),
            transform: Transform {
                scale_x: sx,
                scale_y: sy,
                rotation,
                mirror_x: mx,
                mirror_y: my,
            },
        };
        srm_one(large, dma, timer, console, cfg)?;
        assert!(large.reset_if_unused());
        assert!(dma.reset_if_unused());
    }
    Ok(())
}
pub fn srm_large(
    large: &Arena<'_, External>,
    dma: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    use hal::ppa::{Rect, Rotation, Srm, Transform};
    for (w, h, source_size, destination_size, t) in [
        (66, 17, (96, 32), (96, 32), Transform::default()),
        (80, 33, (96, 48), (96, 48), Transform::default()),
        (1024, 600, (1024, 608), (1024, 600), Transform::default()),
        (
            1024,
            600,
            (1024, 608),
            (600, 1024),
            Transform {
                rotation: Rotation::Deg90,
                ..Transform::default()
            },
        ),
        (
            1024,
            600,
            (1024, 608),
            (512, 300),
            Transform {
                scale_x: 8,
                scale_y: 8,
                ..Transform::default()
            },
        ),
        (
            320,
            240,
            (320, 240),
            (640, 480),
            Transform {
                scale_x: 32,
                scale_y: 32,
                ..Transform::default()
            },
        ),
    ] {
        let cfg = Srm {
            source_size,
            source: Rect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
            destination_size,
            destination: (0, 0),
            transform: t,
        };
        srm_one(large, dma, timer, console, cfg)?;
        assert!(large.reset_if_unused());
        assert!(dma.reset_if_unused());
    }
    Ok(())
}
fn srm_one(
    large: &Arena<'_, External>,
    dma: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    cfg: hal::ppa::Srm,
) -> Result<(), &'static str> {
    use hal::ppa::Rotation;
    let input_len = usize::from(cfg.source_size.0) * usize::from(cfg.source_size.1) * 2;
    let output_len = usize::from(cfg.destination_size.0) * usize::from(cfg.destination_size.1) * 2;
    let mut input = large
        .allocate_paced(Layout::from_size_align(input_len + 128, 64).unwrap())
        .map_err(|_| "input memory")?;
    let mut output = large
        .allocate_paced(Layout::from_size_align(output_len + 128, 64).unwrap())
        .map_err(|_| "output memory")?;
    for b in [&mut input, &mut output] {
        for chunk in b.as_mut_slice().chunks_mut(64) {
            esp32_p4::web::upload::copy_paced(chunk, &[0x5a; 64][..chunk.len()]);
        }
    }
    for (n, chunk) in input.as_mut_slice()[64..64 + input_len]
        .chunks_mut(64)
        .enumerate()
    {
        let mut pixels = [0u8; 64];
        for (i, word) in pixels[..chunk.len()]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .enumerate()
        {
            let at = n * 32 + i;
            *word = pattern(
                (at % usize::from(cfg.source_size.0)) as u16,
                (at / usize::from(cfg.source_size.0)) as u16,
            )
            .to_le_bytes();
        }
        esp32_p4::web::upload::copy_paced(chunk, &pixels[..chunk.len()]);
    }
    let mut desc = dma
        .allocate(Layout::from_size_align(256, 64).unwrap())
        .map_err(|_| "descriptors")?;
    desc.as_mut_slice().fill(0x3c);
    let begin = timer.now_us();
    let result = unsafe {
        hal::ppa::srm_rgb565(
            &input.as_slice()[64..64 + input_len],
            &mut output.as_mut_slice()[64..64 + output_len],
            cfg,
            &mut desc.as_mut_slice()[64..192],
            timer,
        )
    };
    let us = timer.now_us() - begin;
    let _ = writeln!(
        console,
        "PPA SRM cfg={:?} us={} result={:x?}",
        cfg, us, result
    );
    for b in [&input, &output] {
        let bytes = b.as_slice();
        if !bytes[..64]
            .iter()
            .chain(bytes[bytes.len() - 64..].iter())
            .all(|v| *v == 0x5a)
        {
            return Err("SRM buffer guard");
        }
    }
    if !desc.as_slice()[..64]
        .iter()
        .chain(desc.as_slice()[192..].iter())
        .all(|v| *v == 0x3c)
    {
        return Err("SRM descriptor guard");
    }
    result.map_err(|_| "SRM hardware")?;
    let rect = cfg.output_rect().unwrap();
    let mut max_error = [0u16; 3];
    for (i, word) in output.as_slice()[64..64 + output_len]
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
    {
        let x = (i % usize::from(cfg.destination_size.0)) as u16;
        let y = (i / usize::from(cfg.destination_size.0)) as u16;
        let expected =
            if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
                let mut x = x - rect.x;
                let mut y = y - rect.y;
                if cfg.transform.mirror_x {
                    x = rect.width - 1 - x
                }
                if cfg.transform.mirror_y {
                    y = rect.height - 1 - y
                }
                let (w, h) = (
                    (u32::from(cfg.source.width) * u32::from(cfg.transform.scale_x) / 16) as u16,
                    (u32::from(cfg.source.height) * u32::from(cfg.transform.scale_y) / 16) as u16,
                );
                let (sx, sy) = match cfg.transform.rotation {
                    Rotation::Deg0 => (x, y),
                    Rotation::Deg90 => (w - 1 - y, x),
                    Rotation::Deg180 => (w - 1 - x, h - 1 - y),
                    Rotation::Deg270 => (y, h - 1 - x),
                };
                bilinear_pattern(cfg, sx, sy)
            } else {
                0x5a5a
            };
        let actual = u16::from_le_bytes(*word);
        let errors = [
            (actual >> 11).abs_diff(expected >> 11),
            ((actual >> 5) & 63).abs_diff((expected >> 5) & 63),
            (actual & 31).abs_diff(expected & 31),
        ];
        for (i, e) in errors.into_iter().enumerate() {
            max_error[i] = max_error[i].max(e)
        }
        let scaling = cfg.transform.scale_x != 16 || cfg.transform.scale_y != 16;
        let inside =
            x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
        if (actual != expected && (!scaling || !inside))
            || (scaling && errors.into_iter().any(|e| e > 1))
        {
            let _ = writeln!(
                console,
                "PPA SRM mismatch x={} y={} got={:04x} expected={:04x}",
                x,
                y,
                u16::from_le_bytes(*word),
                expected
            );
            return Err("SRM pixel reference");
        }
    }
    for (i, word) in input.as_slice()[64..64 + input_len]
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
    {
        if u16::from_le_bytes(*word)
            != pattern(
                (i % usize::from(cfg.source_size.0)) as u16,
                (i / usize::from(cfg.source_size.0)) as u16,
            )
        {
            return Err("SRM source modified");
        }
    }
    let _ = writeln!(
        console,
        "PPA SRM pixels=true source=true guards=true max_channel_error={:?}",
        max_error
    );
    Ok(())
}

// Independent scalar bilinear reference in native5/6-bit components. Hardware
// converts to ARGB8888 internally, so scaled results permit one quantization step.
fn bilinear_pattern(cfg: hal::ppa::Srm, x: u16, y: u16) -> u16 {
    // Pixel-center mapping: source=(destination+0.5)/scale-0.5.
    let sx = 2 * u32::from(cfg.transform.scale_x);
    let sy = 2 * u32::from(cfg.transform.scale_y);
    let nx = ((2 * i32::from(x) + 1) * 16 - i32::from(cfg.transform.scale_x)).max(0) as u32;
    let ny = ((2 * i32::from(y) + 1) * 16 - i32::from(cfg.transform.scale_y)).max(0) as u32;
    let x0 = (nx / sx).min(u32::from(cfg.source.width) - 1) as u16;
    let y0 = (ny / sy).min(u32::from(cfg.source.height) - 1) as u16;
    let x1 = (x0 + 1).min(cfg.source.width - 1);
    let y1 = (y0 + 1).min(cfg.source.height - 1);
    let fx = nx % sx;
    let fy = ny % sy;
    let colors = [
        pattern(cfg.source.x + x0, cfg.source.y + y0),
        pattern(cfg.source.x + x1, cfg.source.y + y0),
        pattern(cfg.source.x + x0, cfg.source.y + y1),
        pattern(cfg.source.x + x1, cfg.source.y + y1),
    ];
    let weights = [
        (sx - fx) * (sy - fy),
        fx * (sy - fy),
        (sx - fx) * fy,
        fx * fy,
    ];
    let component = |shift: u16, mask: u16| {
        colors
            .into_iter()
            .zip(weights)
            .map(|(c, w)| u32::from((c >> shift) & mask) * w)
            .sum::<u32>()
            / (sx * sy)
    };
    ((component(11, 31) << 11) | (component(5, 63) << 5) | component(0, 31)) as u16
}

pub fn srm_stress(
    large: &Arena<'_, External>,
    dma: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    for _ in 0..10 {
        srm(large, dma, timer, console)?;
    }
    Ok(())
}

fn blend_background(x: u16, y: u16) -> u16 {
    (((x * 3 + y) & 31) << 11) | (((y * 5 + 7) & 63) << 5) | ((x * 2 + y * 7) & 31)
}
fn blend_foreground(x: u16, y: u16) -> u32 {
    let a = [0u32, 1, 64, 127, 128, 192, 254, 255][usize::from((x + 2 * y) % 8)];
    let (r, g, b) = if (x + y).is_multiple_of(17) {
        (0, 0, 0)
    } else {
        (
            u32::from(x * 37 + y * 11) & 255,
            u32::from(x * 13 + y * 29) & 255,
            u32::from(x * 7 + y * 43) & 255,
        )
    };
    (a << 24) | (r << 16) | (g << 8) | b
}
pub fn blend(
    large: &Arena<'_, External>,
    dma: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    probe: bool,
) -> Result<(), &'static str> {
    use hal::ppa::{Alpha, Blend, Rect};
    let mut rid_warning = false;
    for alpha in [
        Alpha::Pixel,
        Alpha::Fixed(0),
        Alpha::Fixed(1),
        Alpha::Fixed(64),
        Alpha::Fixed(128),
        Alpha::Fixed(254),
        Alpha::Fixed(255),
        Alpha::Scale(0),
        Alpha::Scale(1),
        Alpha::Scale(128),
        Alpha::Scale(255),
        Alpha::Scale(256),
        Alpha::Invert,
    ] {
        let cfg = Blend {
            background_size: (32, 24),
            background: Rect {
                x: 4,
                y: 2,
                width: 20,
                height: 12,
            },
            foreground_size: (35, 25),
            foreground: (3, 1),
            destination_size: (64, 32),
            destination: (8, 5),
            alpha,
        };
        match blend_one(large, dma, timer, console, cfg, probe) {
            Err("RID diagnostic: valid pixel data") => rid_warning = true,
            other => other?,
        }
        assert!(large.reset_if_unused());
        assert!(dma.reset_if_unused());
    }
    if rid_warning {
        Err("RID diagnostic: all pixel checks passed")
    } else {
        Ok(())
    }
}
fn blend_one(
    large: &Arena<'_, External>,
    dma: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    cfg: hal::ppa::Blend,
    probe: bool,
) -> Result<(), &'static str> {
    use hal::ppa::Alpha;
    let bg_len = usize::from(cfg.background_size.0) * usize::from(cfg.background_size.1) * 2;
    let fg_len = usize::from(cfg.foreground_size.0) * usize::from(cfg.foreground_size.1) * 4;
    let out_len = usize::from(cfg.destination_size.0) * usize::from(cfg.destination_size.1) * 2;
    let mut bg = large
        .allocate_paced(Layout::from_size_align(bg_len + 128, 64).unwrap())
        .map_err(|_| "background memory")?;
    let mut fg = large
        .allocate_paced(Layout::from_size_align(fg_len + 128, 64).unwrap())
        .map_err(|_| "foreground memory")?;
    let mut output = large
        .allocate_paced(Layout::from_size_align(out_len + 128, 64).unwrap())
        .map_err(|_| "output memory")?;
    for b in [&mut bg, &mut fg, &mut output] {
        for chunk in b.as_mut_slice().chunks_mut(64) {
            esp32_p4::web::upload::copy_paced(chunk, &[0x5a; 64][..chunk.len()]);
        }
    }
    for (n, chunk) in bg.as_mut_slice()[64..64 + bg_len]
        .chunks_mut(64)
        .enumerate()
    {
        let mut bytes = [0u8; 64];
        for (i, word) in bytes[..chunk.len()]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .enumerate()
        {
            let at = n * 32 + i;
            *word = blend_background(
                (at % usize::from(cfg.background_size.0)) as u16,
                (at / usize::from(cfg.background_size.0)) as u16,
            )
            .to_le_bytes();
        }
        esp32_p4::web::upload::copy_paced(chunk, &bytes[..chunk.len()]);
    }
    for (n, chunk) in fg.as_mut_slice()[64..64 + fg_len]
        .chunks_mut(64)
        .enumerate()
    {
        let mut bytes = [0u8; 64];
        for (i, word) in bytes[..chunk.len()]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            let at = n * 16 + i;
            *word = blend_foreground(
                (at % usize::from(cfg.foreground_size.0)) as u16,
                (at / usize::from(cfg.foreground_size.0)) as u16,
            )
            .to_le_bytes();
        }
        esp32_p4::web::upload::copy_paced(chunk, &bytes[..chunk.len()]);
    }
    let mut desc = dma
        .allocate(Layout::from_size_align(320, 64).unwrap())
        .map_err(|_| "blend descriptors")?;
    desc.as_mut_slice().fill(0x3c);
    let begin = timer.now_us();
    let result = unsafe {
        let operation = if probe {
            hal::ppa::probe_blend_rgb565
        } else {
            hal::ppa::blend_rgb565
        };
        operation(
            &bg.as_slice()[64..64 + bg_len],
            &fg.as_slice()[64..64 + fg_len],
            &mut output.as_mut_slice()[64..64 + out_len],
            cfg,
            &mut desc.as_mut_slice()[64..256],
            timer,
        )
    };
    let us = timer.now_us() - begin;
    let _ = writeln!(
        console,
        "PPA BLEND cfg={:?} us={} result={:x?}",
        cfg, us, result
    );
    for b in [&bg, &fg, &output] {
        let s = b.as_slice();
        if !s[..64]
            .iter()
            .chain(s[s.len() - 64..].iter())
            .all(|v| *v == 0x5a)
        {
            return Err("BLEND buffer guard");
        }
    }
    if !desc.as_slice()[..64]
        .iter()
        .chain(desc.as_slice()[256..].iter())
        .all(|v| *v == 0x3c)
    {
        return Err("BLEND descriptor guard");
    }
    let diagnostic_failed = result.is_err();
    if probe {
        if let Err(hal::ppa::Error::Hardware(status)) = &result {
            if status.rx & 2 == 0
                || (status.axi | status.drained | status.aborted | status.axi_observed) & 0xff0 != 0
                || status.ppa & 4 != 0
                || status.blend_errors & 1 != 0
                || status.rx & 0x3ffc != 0
                || (status.tx | status.tx1) & 0x1ff4 != 0
            {
                return Err("BLEND hardware");
            }
        } else {
            result.map_err(|_| "BLEND hardware")?;
        }
    } else {
        result.map_err(|_| "BLEND hardware")?;
    }

    let rect = cfg.output_rect().unwrap();
    let mut max_error = [0u16; 3];
    let mut exact_endpoints = 0;
    for (i, word) in output.as_slice()[64..64 + out_len]
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
    {
        let x = (i % usize::from(cfg.destination_size.0)) as u16;
        let y = (i / usize::from(cfg.destination_size.0)) as u16;
        let actual = u16::from_le_bytes(*word);
        let inside =
            x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
        let (expected, exact) = if inside {
            let (x, y) = (x - rect.x, y - rect.y);
            let b = blend_background(cfg.background.x + x, cfg.background.y + y);
            let f = blend_foreground(cfg.foreground.0 + x, cfg.foreground.1 + y);
            let a = f >> 24;
            let a = match cfg.alpha {
                Alpha::Pixel => a,
                Alpha::Fixed(v) => u32::from(v),
                Alpha::Scale(v) => a * u32::from(v) / 256,
                Alpha::Invert => 255 - a,
            };
            let r = u32::from(b >> 11);
            let g = u32::from((b >> 5) & 63);
            let blue = u32::from(b & 31);
            let background = [
                (r << 3) | (r >> 2),
                (g << 2) | (g >> 4),
                (blue << 3) | (blue >> 2),
            ];
            let foreground = [(f >> 16) & 255, (f >> 8) & 255, f & 255];
            let c = |i: usize| (background[i] * (255 - a) + foreground[i] * a + 127) / 255;
            let value = (((c(0) >> 3) << 11) | ((c(1) >> 2) << 5) | (c(2) >> 3)) as u16;
            (value, a == 0 || a == 255)
        } else {
            (0x5a5a, true)
        };
        let errors = [
            (actual >> 11).abs_diff(expected >> 11),
            ((actual >> 5) & 63).abs_diff((expected >> 5) & 63),
            (actual & 31).abs_diff(expected & 31),
        ];
        for (i, e) in errors.into_iter().enumerate() {
            max_error[i] = max_error[i].max(e)
        }
        if exact && inside {
            exact_endpoints += 1;
        }
        if (exact && actual != expected) || errors.into_iter().any(|e| e > 1) {
            let _ = writeln!(
                console,
                "PPA BLEND mismatch x={} y={} actual={:04x} expected={:04x} exact={}",
                x, y, actual, expected, exact
            );
            return Err("BLEND pixel reference");
        }
    }
    for (i, word) in bg.as_slice()[64..64 + bg_len]
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
    {
        if u16::from_le_bytes(*word)
            != blend_background(
                (i % usize::from(cfg.background_size.0)) as u16,
                (i / usize::from(cfg.background_size.0)) as u16,
            )
        {
            return Err("BLEND background modified");
        }
    }
    for (i, word) in fg.as_slice()[64..64 + fg_len]
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
    {
        if u32::from_le_bytes(*word)
            != blend_foreground(
                (i % usize::from(cfg.foreground_size.0)) as u16,
                (i / usize::from(cfg.foreground_size.0)) as u16,
            )
        {
            return Err("BLEND foreground modified");
        }
    }
    let _ = writeln!(
        console,
        "PPA BLEND pixels=true sources=true guards=true max_channel_error={:?} exact_endpoints={}",
        max_error, exact_endpoints
    );
    if diagnostic_failed {
        return Err("RID diagnostic: valid pixel data");
    }
    Ok(())
}

pub fn blend_large(
    large: &Arena<'_, External>,
    dma: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    probe: bool,
) -> Result<(), &'static str> {
    use hal::ppa::{Alpha, Blend, Rect};
    let mut rid_warning = false;
    for (width, height, alpha) in [
        (2, 1, Alpha::Pixel),
        (320, 240, Alpha::Fixed(128)),
        (1024, 600, Alpha::Pixel),
        (1024, 600, Alpha::Scale(128)),
    ] {
        let cfg = Blend {
            background_size: (width, height),
            background: Rect {
                x: 0,
                y: 0,
                width,
                height,
            },
            foreground_size: (width, height),
            foreground: (0, 0),
            destination_size: (width, height),
            destination: (0, 0),
            alpha,
        };
        match blend_one(large, dma, timer, console, cfg, probe) {
            Err("RID diagnostic: valid pixel data") => rid_warning = true,
            other => other?,
        }
        assert!(large.reset_if_unused());
        assert!(dma.reset_if_unused());
    }
    if rid_warning {
        Err("RID diagnostic: all pixel checks passed")
    } else {
        Ok(())
    }
}

pub fn blend_stress_probe(
    large: &Arena<'_, External>,
    dma: &Arena<'_, Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    let mut rid_warning = false;
    for _ in 0..10 {
        match blend(large, dma, timer, console, true) {
            Err("RID diagnostic: all pixel checks passed") => rid_warning = true,
            other => other?,
        }
    }
    if rid_warning {
        Err("RID diagnostic: all pixel checks passed")
    } else {
        Ok(())
    }
}
