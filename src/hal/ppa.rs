//! Experimental rev1.3 PPA: RGB565 FILL and SRM, no LCD ownership.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}
impl Rect {
    pub fn fits(self, width: u16, height: u16) -> bool {
        width != 0
            && height != 0
            && width <= 0x3fff
            && height <= 0x3fff
            && width & 1 == 0
            && self.x & 1 == 0
            && self.width & 1 == 0
            && self.width != 0
            && self.height != 0
            && self.x < width
            && self.y < height
            && self.width <= width - self.x
            && self.height <= height - self.y
    }
}
/// Rotation is counterclockwise, applied after scaling, before mirrors.
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum Rotation {
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}
#[derive(Debug, Clone, Copy)]
pub struct Transform {
    /// Scale in sixteenths:16 is unity,1 is1/16,4095 is255+15/16.
    pub scale_x: u16,
    pub scale_y: u16,
    pub rotation: Rotation,
    pub mirror_x: bool,
    pub mirror_y: bool,
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            scale_x: 16,
            scale_y: 16,
            rotation: Rotation::Deg0,
            mirror_x: false,
            mirror_y: false,
        }
    }
}
impl Transform {
    pub fn extent(self, width: u16, height: u16) -> Option<(u16, u16)> {
        if width == 0
            || height == 0
            || !(1..=4095).contains(&self.scale_x)
            || !(1..=4095).contains(&self.scale_y)
        {
            return None;
        }
        let w = u32::from(width) * u32::from(self.scale_x) / 16;
        let h = u32::from(height) * u32::from(self.scale_y) / 16;
        if w == 0 || h == 0 || w > 8191 || h > 8191 {
            return None;
        }
        Some(match self.rotation {
            Rotation::Deg90 | Rotation::Deg270 => (h as u16, w as u16),
            _ => (w as u16, h as u16),
        })
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Srm {
    pub source_size: (u16, u16),
    pub source: Rect,
    pub destination_size: (u16, u16),
    pub destination: (u16, u16),
    pub transform: Transform,
}
impl Srm {
    pub fn output_rect(self) -> Option<Rect> {
        if !self.source.fits(self.source_size.0, self.source_size.1) {
            return None;
        }
        let (width, height) = self
            .transform
            .extent(self.source.width, self.source.height)?;
        let r = Rect {
            x: self.destination.0,
            y: self.destination.1,
            width,
            height,
        };
        r.fits(self.destination_size.0, self.destination_size.1)
            .then_some(r)
    }
}
/// Straight (not premultiplied) foreground alpha policy.
#[derive(Debug, Clone, Copy)]
pub enum Alpha {
    Pixel,
    Fixed(u8),
    /// Multiply pixel alpha by value/256;256 is exactly Pixel,0 is transparent.
    Scale(u16),
    Invert,
}
#[derive(Debug, Clone, Copy)]
pub struct Blend {
    pub background_size: (u16, u16),
    pub background: Rect,
    pub foreground_size: (u16, u16),
    pub foreground: (u16, u16),
    pub destination_size: (u16, u16),
    pub destination: (u16, u16),
    pub alpha: Alpha,
}
impl Blend {
    pub fn output_rect(self) -> Option<Rect> {
        let r = self.background;
        if !r.fits(self.background_size.0, self.background_size.1) {
            return None;
        }
        let (fw, fh) = self.foreground_size;
        let (fx, fy) = self.foreground;
        // ARGB8888 rows/x already have natural4B alignment, odd x is allowed.
        if fw == 0
            || fh == 0
            || fw > 0x3fff
            || fh > 0x3fff
            || fx >= fw
            || fy >= fh
            || r.width > fw - fx
            || r.height > fh - fy
        {
            return None;
        }
        if let Alpha::Scale(value) = self.alpha {
            if value > 256 {
                return None;
            }
        }
        let out = Rect {
            x: self.destination.0,
            y: self.destination.1,
            width: r.width,
            height: r.height,
        };
        out.fits(self.destination_size.0, self.destination_size.1)
            .then_some(out)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub ppa: u32,
    pub tx: u32,
    pub tx1: u32,
    pub tx1_state: u32,
    pub tx_fifo: u32,
    pub tx1_fifo: u32,
    pub startup_axi: [u32; 3],
    /// OR of observed AXI error fields during this operation, not a count.
    pub axi_observed: u32,
    pub tx_state: u32,
    pub srm: u32,
    pub parameter_errors: u32,
    pub blend_errors: u32,
    pub rx: u32,
    pub axi: u32,
    pub drained: u32,
    pub aborted: u32,
    pub mode: u32,
    pub geometry: u32,
    pub rx_state: u32,
    pub descriptor: u32,
}
#[derive(Debug)]
pub enum Error {
    Memory,
    Geometry,
    Cache,
    Busy,
    Hardware(Status),
    Timeout(Status),
}
#[cfg(all(target_arch = "riscv32", feature = "ppa-lab"))]
mod hardware {
    use super::*;
    use crate::hal::{
        dma2d::{bus_address, Claim},
        time::Timer,
    };
    use core::arch::asm;
    const PPA: usize = 0x50087000;
    const DMA: usize = 0x50088000;
    const RX: usize = DMA + 0x500;
    const CLK: usize = 0x500e6018;
    const RESET: usize = 0x500e60c0;
    fn r(a: usize) -> u32 {
        unsafe { (a as *const u32).read_volatile() }
    }
    fn w(a: usize, v: u32) {
        unsafe { (a as *mut u32).write_volatile(v) }
    }
    fn fence() {
        unsafe { asm!("fence iorw,iorw", options(nostack)) }
    }
    fn modify(a: usize, clear: u32, set: u32) {
        w(a, (r(a) & !clear) | set)
    }
    fn pulse(a: usize, bits: u32) {
        modify(a, 0, bits);
        fence();
        modify(a, bits, 0);
        fence()
    }
    fn status() -> Status {
        Status {
            ppa: r(PPA + 0x10),
            tx: r(DMA + 4),
            tx1: r(DMA + 0x104),
            tx1_state: r(DMA + 0x124),
            tx_fifo: r(DMA + 0x14),
            tx1_fifo: r(DMA + 0x114),
            startup_axi: [0; 3],
            axi_observed: 0,
            tx_state: r(DMA + 0x24),
            srm: r(PPA + 0x64),
            parameter_errors: r(PPA + 0x78),
            blend_errors: r(PPA + 0x74),
            rx: r(RX + 4),
            axi: r(DMA + 0xa00),
            drained: 0,
            aborted: 0,
            mode: r(PPA + 0x34),
            geometry: r(PPA + 0x3c),
            rx_state: r(RX + 0x24),
            descriptor: r(RX + 0x2c),
        }
    }
    /// # Safety
    /// HP0 owns PPA and all 2D-DMA channels; no other codec/PPA operations.
    /// Output is CPU-owned HP SRAM or uncached PSRAM; descriptor workspace is
    /// separate, >=64B, aligned64 and CPU-owned. Neither may be LCD/other-DMA owned.
    /// D-cache OFF, L2=0; buffers remain owned until quiesce on every return path.
    pub unsafe fn fill_rgb565(
        output: &mut [u8],
        width: u16,
        height: u16,
        rect: Rect,
        color: u16,
        descriptors: &mut [u8],
        timer: &mut Timer,
    ) -> Result<Status, Error> {
        if !rect.fits(width, height) {
            return Err(Error::Geometry);
        }
        let required = usize::from(width) * usize::from(height) * 2;
        if output.len() < required || descriptors.len() < 64 {
            return Err(Error::Memory);
        }
        let target = bus_address(output.as_ptr() as usize, output.len()).ok_or(Error::Memory)?;
        let desc =
            bus_address(descriptors.as_ptr() as usize, descriptors.len()).ok_or(Error::Memory)?;
        let cache = crate::hal::memory::cache_snapshot();
        if cache.l1_bypass & 16 == 0 || cache.l2_size != 0 {
            return Err(Error::Cache);
        }
        let _claim = Claim::acquire().ok_or(Error::Busy)?;
        let clock_bits = (1 << 6) | (1 << 9);
        let old = r(CLK);
        modify(CLK, 0, clock_bits);
        pulse(RESET, 1 << 31);
        pulse(RESET + 4, 1);
        w(DMA + 0xa04, 4);
        pulse(DMA + 0xa04, 1);
        pulse(DMA + 0xa04, 2);
        w(DMA + 0xa18, 0);
        w(DMA + 0xa1c, 0);
        let words = [
            u32::from(rect.height) | (u32::from(rect.width) << 14) | (7 << 29),
            u32::from(height) | (u32::from(width) << 14) | (3 << 28),
            u32::from(rect.y) | (u32::from(rect.x) << 14),
            target,
            0,
            0,
        ];
        for (i, v) in words.into_iter().enumerate() {
            w(descriptors.as_mut_ptr() as usize + i * 4, v)
        }
        w(RX, (1 << 2) | (2 << 6) | (3 << 9) | (1 << 12));
        w(RX + 0x3c, 2);
        w(RX + 8, 0);
        w(RX + 0x10, 0x3fff);
        w(RX + 0x4c, (7 << 3) | 1);
        w(RX + 0x50, 0);
        w(PPA + 0x18, 0);
        w(PPA + 0x1c, 7);
        w(PPA + 0x24, 2 << 8);
        w(
            PPA + 0x3c,
            u32::from(rect.width) | (u32::from(rect.height) << 14),
        );
        // FILL's constant pixel is ARGB8888 even when its output is RGB565.
        let c = u32::from(color);
        let red = c >> 11;
        let green = (c >> 5) & 63;
        let blue = c & 31;
        let argb = 0xff00_0000
            | (((red << 3) | (red >> 2)) << 16)
            | (((green << 2) | (green >> 4)) << 8)
            | (blue << 3)
            | (blue >> 2);
        w(PPA + 0x4c, argb);
        w(RX + 0x20, desc);
        fence();
        w(RX + 0x1c, 1 << 22);
        fence();
        w(PPA + 0x34, 1 | 4);
        fence();
        w(PPA + 0x34, 1 | 4 | 8);
        fence();
        complete(timer, old, Engine::Fill, true)
    }
    /// # Safety
    /// Same exclusive DMA/PPA/cache and lifetime requirements as fill_rgb565.
    /// Input, output and descriptors are separate, CPU-owned regions; no aliases
    /// (including cached/uncached aliases). Workspace >=128B,64B-aligned.
    pub unsafe fn srm_rgb565(
        input: &[u8],
        output: &mut [u8],
        cfg: Srm,
        descriptors: &mut [u8],
        timer: &mut Timer,
    ) -> Result<Status, Error> {
        let _out = cfg.output_rect().ok_or(Error::Geometry)?;
        if input.len() < usize::from(cfg.source_size.0) * usize::from(cfg.source_size.1) * 2
            || output.len()
                < usize::from(cfg.destination_size.0) * usize::from(cfg.destination_size.1) * 2
            || descriptors.len() < 128
        {
            return Err(Error::Memory);
        }
        let source = bus_address(input.as_ptr() as usize, input.len()).ok_or(Error::Memory)?;
        let target = bus_address(output.as_ptr() as usize, output.len()).ok_or(Error::Memory)?;
        let desc =
            bus_address(descriptors.as_ptr() as usize, descriptors.len()).ok_or(Error::Memory)?;
        let overlaps = |a: u32, n: usize, b: u32, m: usize| {
            u64::from(a) < u64::from(b) + m as u64 && u64::from(b) < u64::from(a) + n as u64
        };
        if overlaps(source, input.len(), target, output.len())
            || overlaps(source, input.len(), desc, descriptors.len())
            || overlaps(target, output.len(), desc, descriptors.len())
        {
            return Err(Error::Memory);
        }
        let cache = crate::hal::memory::cache_snapshot();
        if cache.l1_bypass & 16 == 0 || cache.l2_size != 0 {
            return Err(Error::Cache);
        }
        let _claim = Claim::acquire().ok_or(Error::Busy)?;
        let old = r(CLK);
        modify(CLK, 0, (1 << 6) | (1 << 9));
        pulse(RESET, 1 << 31);
        pulse(RESET + 4, 1);
        w(DMA + 0xa04, 4);
        pulse(DMA + 0xa04, 1);
        pulse(DMA + 0xa04, 2);
        w(DMA + 0xa18, 0);
        w(DMA + 0xa1c, 0);
        let src = cfg.source;
        let tx = [
            u32::from(src.height) | (u32::from(src.width) << 14) | (7 << 29),
            u32::from(cfg.source_size.1) | (u32::from(cfg.source_size.0) << 14) | (3 << 28),
            u32::from(src.y) | (u32::from(src.x) << 14),
            source,
            0,
            0,
        ];
        // DSCR-PORT lets SRM supply each destination block's dimensions.
        let rx = [
            2 | (2 << 14) | (7 << 29),
            u32::from(cfg.destination_size.1)
                | (u32::from(cfg.destination_size.0) << 14)
                | (3 << 28),
            u32::from(cfg.destination.1) | (u32::from(cfg.destination.0) << 14),
            target,
            0,
            0,
        ];
        for (offset, words) in [(0, tx), (64, rx)] {
            for (i, v) in words.into_iter().enumerate() {
                w(descriptors.as_mut_ptr() as usize + offset + i * 4, v)
            }
        }
        w(
            DMA,
            (1 << 1) | (1 << 2) | (2 << 6) | (3 << 9) | (1 << 11) | (1 << 12),
        );
        w(RX, (1 << 2) | (2 << 6) | (3 << 9) | (1 << 11) | (1 << 12));
        w(DMA + 0x38, 1);
        w(RX + 0x3c, 1);
        w(DMA + 8, 0);
        w(RX + 8, 0);
        w(DMA + 0x10, 0x1fff);
        w(RX + 0x10, 0x3fff);
        w(DMA + 0x48, (7 << 3) | 2);
        w(RX + 0x4c, (7 << 3) | 1);
        w(RX + 0x50, 0);
        w(DMA + 0x6c, 18 | (18 << 14));
        w(PPA + 0x18, 0);
        w(PPA + 0x1c, 7);
        w(PPA + 0x20, 2 | (2 << 4));
        w(PPA + 0x38, 0);
        let t = cfg.transform;
        // DIG-734 workaround from IDF: bypass block concatenation for a short
        // last transfer unit that cannot fill the DMA2D receive FIFO.
        let w_out = u32::from(src.width) * u32::from(t.scale_x) / 16;
        let w_left = if w_out % 64 == 0 { 64 } else { w_out % 64 };
        let h_in = if src.height.is_multiple_of(16) {
            16
        } else {
            u32::from(src.height % 16)
        };
        let h_left = h_in * u32::from(t.scale_y) / 16;
        let bypass = (w_out > 64 || src.height > 16) && w_left * h_left * 16 < 12 * 128;
        w(PPA + 0x28, if bypass { 1 << 2 } else { 0 });
        let control = u32::from(t.scale_x / 16)
            | (u32::from(t.scale_x % 16) << 8)
            | (u32::from(t.scale_y / 16) << 12)
            | (u32::from(t.scale_y % 16) << 20)
            | ((t.rotation as u32) << 24)
            | ((t.mirror_x as u32) << 28)
            | ((t.mirror_y as u32) << 29);
        w(PPA + 0x64, control);
        w(DMA + 0x20, desc);
        w(RX + 0x20, desc + 64);
        fence();
        w(DMA + 0x1c, 1 << 21);
        fence();
        // Serialize initial descriptor reads. Unlike JPEG, SRM cannot prefill
        // pixels until it requests a block through DSCR-PORT.
        let begin = timer.now_us();
        let mut ready = true;
        while r(DMA + 0x24) & 0x0003ffff != desc & 0x0003ffff || r(DMA + 0xa00) & 0x0007f000 != 0 {
            if timer.now_us() - begin >= 100_000 {
                ready = false;
                break;
            }
        }
        fence();
        if ready {
            w(RX + 0x1c, 1 << 22);
            fence();
            w(PPA + 0x64, control | (1 << 27));
            fence();
        }
        complete(timer, old, Engine::Srm, ready)
    }
    /// # Safety
    /// HP0 exclusively owns PPA/all2D-DMA, D-cache OFF/L2=0. All four regions
    /// are distinct CPU-owned SRAM/uncached PSRAM; input/output must not be LCD
    /// or other-DMA owned. Descriptor workspace>=192B,64B-aligned. Straight ARGB.
    pub unsafe fn blend_rgb565(
        background: &[u8],
        foreground: &[u8],
        output: &mut [u8],
        cfg: Blend,
        descriptors: &mut [u8],
        timer: &mut Timer,
    ) -> Result<Status, Error> {
        unsafe {
            blend_inner(
                background,
                foreground,
                output,
                cfg,
                descriptors,
                timer,
                false,
            )
        }
    }
    /// Diagnostic-only: defer RID failure until EOF, to inspect pixel data.
    /// All observed AXI errors still make the final result Err; no display use.
    /// # Safety
    /// Same region, ownership and cache requirements as blend_rgb565.
    pub unsafe fn probe_blend_rgb565(
        background: &[u8],
        foreground: &[u8],
        output: &mut [u8],
        cfg: Blend,
        descriptors: &mut [u8],
        timer: &mut Timer,
    ) -> Result<Status, Error> {
        unsafe {
            blend_inner(
                background,
                foreground,
                output,
                cfg,
                descriptors,
                timer,
                true,
            )
        }
    }
    unsafe fn blend_inner(
        background: &[u8],
        foreground: &[u8],
        output: &mut [u8],
        cfg: Blend,
        descriptors: &mut [u8],
        timer: &mut Timer,
        probe: bool,
    ) -> Result<Status, Error> {
        let out = cfg.output_rect().ok_or(Error::Geometry)?;
        let picture_bytes = |(w, h): (u16, u16), bpp: usize| usize::from(w) * usize::from(h) * bpp;
        if background.len() < picture_bytes(cfg.background_size, 2)
            || foreground.len() < picture_bytes(cfg.foreground_size, 4)
            || output.len() < picture_bytes(cfg.destination_size, 2)
            || descriptors.len() < 192
        {
            return Err(Error::Memory);
        }
        let bg =
            bus_address(background.as_ptr() as usize, background.len()).ok_or(Error::Memory)?;
        let fg =
            bus_address(foreground.as_ptr() as usize, foreground.len()).ok_or(Error::Memory)?;
        let target = bus_address(output.as_ptr() as usize, output.len()).ok_or(Error::Memory)?;
        let desc =
            bus_address(descriptors.as_ptr() as usize, descriptors.len()).ok_or(Error::Memory)?;
        let regions = [
            (bg, background.len()),
            (fg, foreground.len()),
            (target, output.len()),
            (desc, descriptors.len()),
        ];
        for i in 0..regions.len() {
            for j in 0..i {
                let (a, n) = regions[i];
                let (b, m) = regions[j];
                if u64::from(a) < u64::from(b) + m as u64 && u64::from(b) < u64::from(a) + n as u64
                {
                    return Err(Error::Memory);
                }
            }
        }
        let cache = crate::hal::memory::cache_snapshot();
        if cache.l1_bypass & 16 == 0 || cache.l2_size != 0 {
            return Err(Error::Cache);
        }
        let _claim = Claim::acquire().ok_or(Error::Busy)?;
        let old = r(CLK);
        modify(CLK, 0, (1 << 6) | (1 << 9));
        pulse(RESET, 1 << 31);
        pulse(RESET + 4, 1);
        w(DMA + 0xa04, 4);
        pulse(DMA + 0xa04, 1);
        pulse(DMA + 0xa04, 2);
        w(DMA + 0xa18, 0);
        w(DMA + 0xa1c, 0);
        for (offset, size, origin, buffer, pbyte) in [
            (
                0,
                cfg.background_size,
                (cfg.background.x, cfg.background.y),
                bg,
                3,
            ),
            (64, cfg.foreground_size, cfg.foreground, fg, 5),
            (128, cfg.destination_size, cfg.destination, target, 3),
        ] {
            let words = [
                u32::from(out.height) | (u32::from(out.width) << 14) | (7 << 29),
                u32::from(size.1) | (u32::from(size.0) << 14) | (pbyte << 28),
                u32::from(origin.1) | (u32::from(origin.0) << 14),
                buffer,
                0,
                0,
            ];
            for (i, v) in words.into_iter().enumerate() {
                w(descriptors.as_mut_ptr() as usize + offset + i * 4, v)
            }
        }
        for (channel, peripheral) in [(0, 2), (1, 3)] {
            let ch = DMA + channel * 0x100;
            w(ch, (1 << 1) | (1 << 2) | (2 << 6) | (3 << 9) | (1 << 12));
            w(ch + 0x38, peripheral);
            w(ch + 8, 0);
            w(ch + 0x10, 0x1fff);
        }
        w(DMA + 0x48, (7 << 3) | 2);
        w(DMA + 0x4c, 0);
        w(RX, (1 << 2) | (2 << 6) | (3 << 9) | (1 << 12));
        w(RX + 0x3c, 2);
        w(RX + 8, 0);
        w(RX + 0x10, 0x3fff);
        w(RX + 0x4c, (7 << 3) | 1);
        w(RX + 0x50, 0);
        w(PPA + 0x18, 0);
        w(PPA + 0x1c, 7);
        w(PPA + 0x24, 2 | (2 << 8));
        w(PPA + 0x2c, 0);
        let alpha = match cfg.alpha {
            Alpha::Pixel | Alpha::Scale(256) => 0,
            Alpha::Fixed(value) => (1 << 18) | (u32::from(value) << 8),
            Alpha::Scale(value) => (2 << 18) | (u32::from(value) << 8),
            Alpha::Invert => 1 << 21,
        };
        // Opaque background; preserve/replace/scale only the foreground alpha.
        w(PPA + 0x40, 255 | (1 << 16) | alpha);
        w(
            PPA + 0x3c,
            u32::from(out.width) | (u32::from(out.height) << 14),
        );
        // Disable color-key windows: low>high, including true black pixels.
        w(PPA + 0x50, 0x00ff_ffff);
        w(PPA + 0x54, 0);
        w(PPA + 0x58, 0x00ff_ffff);
        w(PPA + 0x5c, 0);
        w(PPA + 0x60, 0);
        w(DMA + 0x20, desc);
        w(DMA + 0x120, desc + 64);
        w(RX + 0x20, desc + 128);
        fence();
        let mut ready = true;
        let mut startup_axi = [0u32; 3];
        for (channel, phase_axi) in startup_axi[..2].iter_mut().enumerate() {
            let ch = DMA + channel * 0x100;
            w(ch + 0x1c, 1 << 21);
            fence();
            let begin = timer.now_us();
            while (r(ch + 4) & 1 == 0 && r(ch + 0x14) & (1 << 23) != 0)
                || r(DMA + 0xa00) & 0x0007f000 != 0
            {
                if timer.now_us() - begin >= 100_000 {
                    ready = false;
                    break;
                }
            }
            *phase_axi = r(DMA + 0xa00);
            if !ready {
                break;
            }
        }
        fence();
        if ready {
            w(RX + 0x1c, 1 << 22);
            fence();
            let begin = timer.now_us();
            while r(RX + 0x24) & 0x0003ffff != (desc + 128) & 0x0003ffff
                || r(DMA + 0xa00) & 0x0007f000 != 0
            {
                if timer.now_us() - begin >= 100_000 {
                    ready = false;
                    break;
                }
            }
            startup_axi[2] = r(DMA + 0xa00);
            fence();
        }
        if ready {
            w(PPA + 0x34, 1);
            fence();
            w(PPA + 0x34, 1 | 8);
            fence();
        }
        let mut result = complete(
            timer,
            old,
            if probe {
                Engine::BlendProbe
            } else {
                Engine::Blend
            },
            ready,
        );
        if let Ok(s) | Err(Error::Hardware(s)) | Err(Error::Timeout(s)) = &mut result {
            s.startup_axi = startup_axi;
            s.axi_observed |= startup_axi.into_iter().fold(0, |acc, v| acc | (v & 0xfff));
        }
        if let Ok(s) = &result {
            if s.axi_observed != 0 {
                result = Err(Error::Hardware(*s));
            }
        }
        result
    }
    enum Engine {
        Fill,
        Srm,
        Blend,
        BlendProbe,
    }
    fn complete(
        timer: &mut Timer,
        old: u32,
        engine: Engine,
        startup_ready: bool,
    ) -> Result<Status, Error> {
        let tx_count = match engine {
            Engine::Fill => 0,
            Engine::Srm => 1,
            Engine::Blend | Engine::BlendProbe => 2,
        };
        let start = timer.now_us();
        let mut observed_axi = 0u32;
        let mut result = loop {
            let mut s = status();
            observed_axi |= s.axi & 0xfff;
            s.axi_observed = observed_axi;
            if s.ppa & 4 != 0
                || (tx_count == 2 && s.blend_errors & 1 != 0)
                || s.rx & 0x3ffc != 0
                || s.axi
                    & if matches!(engine, Engine::BlendProbe) {
                        0xff0
                    } else {
                        0xfff
                    }
                    != 0
                || (tx_count > 0 && s.tx & 0x1ff4 != 0)
                || (tx_count > 1 && s.tx1 & 0x1ff4 != 0)
            {
                break Err(Error::Hardware(s));
            }
            if s.rx & 2 != 0 {
                break Ok(s);
            }
            if !startup_ready || timer.now_us() - start > 1_000_000 {
                break Err(Error::Timeout(s));
            }
            crate::worker::poll();
        };
        let drain = timer.now_us();
        if result.is_ok() {
            while r(DMA + 0xa00) & 0x03f80000 != 0 {
                assert!(
                    timer.now_us() - drain < 100_000,
                    "PPA write drain failed; keep output reserved"
                );
            }
        }
        let drained = r(DMA + 0xa00);
        // Stop the pixel producer, then confirm AXI reset safety before releasing.
        w(PPA + 0x34, 1 << 4);
        if matches!(engine, Engine::Srm) {
            w(PPA + 0x64, 1 << 26);
        }
        for channel in 0..tx_count {
            modify(DMA + channel * 0x100, 0, 1 << 25);
        }
        modify(RX, 0, 1 << 25);
        fence();
        let begin = timer.now_us();
        while r(RX + 0x24) & (1 << 23) == 0
            || (0..tx_count).any(|channel| r(DMA + channel * 0x100 + 0x24) & (1 << 24) == 0)
        {
            assert!(
                timer.now_us() - begin < 100_000,
                "PPA DMA abort failed; keep output reserved"
            );
        }
        let aborted = r(DMA + 0xa00);
        if let Ok(s) | Err(Error::Hardware(s)) | Err(Error::Timeout(s)) = &mut result {
            s.drained = drained;
            s.aborted = aborted;
        }
        if let Ok(s) = &result {
            if (s.axi_observed | s.axi | drained | aborted) & 0xfff != 0 {
                result = Err(Error::Hardware(*s));
            }
        }
        pulse(RX, 1 << 24);
        pulse(RESET, 1 << 31);
        pulse(RESET + 4, 1);
        let clock_bits = (1 << 6) | (1 << 9);
        modify(CLK, clock_bits, old & clock_bits);
        fence();
        result
    }
}
#[cfg(all(target_arch = "riscv32", feature = "ppa-lab"))]
pub use hardware::{blend_rgb565, fill_rgb565, probe_blend_rgb565, srm_rgb565};
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blend_geometry_alpha_and_argb_alignment() {
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
            alpha: Alpha::Pixel,
        };
        let out = cfg.output_rect().unwrap();
        assert_eq!((out.x, out.y, out.width, out.height), (8, 5, 20, 12));
        assert!(Blend {
            foreground: (16, 1),
            ..cfg
        }
        .output_rect()
        .is_none());
        assert!(Blend {
            destination: (48, 5),
            ..cfg
        }
        .output_rect()
        .is_none());
        assert!(Blend {
            destination: (9, 5),
            ..cfg
        }
        .output_rect()
        .is_none());
        assert!(Blend {
            foreground_size: (0, 25),
            ..cfg
        }
        .output_rect()
        .is_none());
        assert!(Blend {
            alpha: Alpha::Scale(257),
            ..cfg
        }
        .output_rect()
        .is_none());
        assert!(Blend {
            alpha: Alpha::Scale(256),
            ..cfg
        }
        .output_rect()
        .is_some());
        assert!(Blend {
            alpha: Alpha::Fixed(255),
            ..cfg
        }
        .output_rect()
        .is_some());
    }
    #[test]
    fn quantized_extents_and_capacity() {
        let t = Transform {
            scale_x: 19,
            scale_y: 13,
            ..Transform::default()
        };
        assert_eq!(t.extent(14, 18), Some((16, 14)));
        assert_eq!(
            Transform {
                rotation: Rotation::Deg90,
                ..t
            }
            .extent(14, 18),
            Some((14, 16))
        );
        assert_eq!(Transform { scale_x: 1, ..t }.extent(14, 18), None);
        assert_eq!(Transform { scale_y: 0, ..t }.extent(14, 18), None);
        assert_eq!(Transform { scale_x: 4096, ..t }.extent(14, 18), None);
        assert_eq!(Transform { scale_x: 4095, ..t }.extent(64, 18), None);
        let cfg = Srm {
            source_size: (32, 32),
            source: Rect {
                x: 0,
                y: 0,
                width: 14,
                height: 18,
            },
            destination_size: (16, 14),
            destination: (0, 0),
            transform: t,
        };
        let r = cfg.output_rect().unwrap();
        assert_eq!((r.width, r.height), (16, 14));
        assert!(Srm {
            destination: (2, 0),
            ..cfg
        }
        .output_rect()
        .is_none());
        assert!(Srm {
            transform: Transform { scale_x: 18, ..t },
            ..cfg
        }
        .output_rect()
        .is_none()); // 15px width rejected by current RGB565 alignment contract
    }
    #[test]
    fn rectangle_bounds_and_alignment() {
        assert!(Rect {
            x: 4,
            y: 3,
            width: 20,
            height: 17
        }
        .fits(64, 32));
        for r in [
            Rect {
                x: 63,
                y: 0,
                width: 2,
                height: 1,
            },
            Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 1,
            },
            Rect {
                x: 2,
                y: 31,
                width: 64,
                height: 2,
            },
            Rect {
                x: 1,
                y: 0,
                width: 2,
                height: 1,
            },
        ] {
            assert!(!r.fits(64, 32));
        }
        assert!(!Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1
        }
        .fits(0xffff, 32));
    }
}
