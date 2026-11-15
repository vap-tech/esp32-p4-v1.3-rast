//! Sole filesystem owner feeds offscreen JPEG/SRM and paced permanent framebuffer writes.
use super::*;
use core::alloc::Layout;
use esp32_p4::{
    devices::framebuffer,
    memory::{Buffer, External, Internal},
    photoframe::{is_jpeg, Options},
    storage::{
        files::OpenMode,
        volume::{Kind, PATH_BYTES},
    },
};
const MAX_FILES: usize = 64;
const RECORD: usize = PATH_BYTES + 2;
const META: usize = 65536;
const INPUT: usize = 2 * 1024 * 1024;
const DECODED: usize = 8 * 1024 * 1024;
const GUARD: usize = 64;
const INPUT_AT: usize = META + GUARD;
const DECODE_AT: usize = INPUT_AT + INPUT + 2 * GUARD;
const FRAMES_AT: usize = DECODE_AT + DECODED + 2 * GUARD;
const FRAME_SPAN: usize = framebuffer::BYTES + 2 * GUARD;
const WORKSPACE: usize = FRAMES_AT + 4 * FRAME_SPAN;
struct Cache<'a> {
    bytes: Buffer<'a, External>,
    descriptors: Buffer<'a, Internal>,
}
pub(super) struct Frame<'a> {
    cache: Option<Cache<'a>>,
    spare_descriptors: Option<Buffer<'a, Internal>>,
    folder: http::Text<PATH_BYTES>,
    file: http::Text<PATH_BYTES>,
    pub running: bool,
    has_picture: bool,
    options: Options,
    count: usize,
    index: usize,
    shown: u32,
    skipped: u32,
    failed: usize,
    next: u64,
    deadline: u64,
    front: usize,
    transition: Option<(usize, u8, bool)>,
    message: &'static str,
}
impl Default for Frame<'_> {
    fn default() -> Self {
        Self {
            cache: None,
            spare_descriptors: None,
            folder: http::Text::new(),
            file: http::Text::new(),
            running: false,
            has_picture: false,
            options: Options {
                interval: 10,
                seconds: 0,
                fill: false,
                effects: 0,
            },
            count: 0,
            index: 0,
            shown: 0,
            skipped: 0,
            failed: 0,
            next: 0,
            deadline: 0,
            front: 0,
            transition: None,
            message: "idle",
        }
    }
}
impl<'a> Frame<'a> {
    pub fn status(&self, io: &mut impl Transport) -> Result<(), web::Error> {
        let mut body = http::Text::<2048>::new();
        write!(body, "{{\"running\":{},\"folder\":", self.running)
            .map_err(|_| web::Error::Format)?;
        http::json_string(&mut body, core::str::from_utf8(self.folder.data()).unwrap())
            .map_err(|_| web::Error::Format)?;
        write!(body, ",\"file\":").map_err(|_| web::Error::Format)?;
        http::json_string(&mut body, core::str::from_utf8(self.file.data()).unwrap())
            .map_err(|_| web::Error::Format)?;
        write!(body,",\"count\":{},\"shown\":{},\"skipped\":{},\"message\":\"{}\",\"interval\":{},\"seconds\":{},\"effects\":{},\"fill\":{}}}",self.count,self.shown,self.skipped,self.message,self.options.interval,self.options.seconds,self.options.effects,self.options.fill).map_err(|_|web::Error::Format)?;
        web::json(io, body.data())
    }
    pub fn start<P: Port, D: disk::BlockDevice, const N: usize>(
        &mut self,
        path: &str,
        options: Options,
        fs: &mut FileTable<'_, '_, D, N>,
        network: &RefCell<Network<'a, '_, '_, P>>,
        external: &'a memory::Arena<'_, External>,
        working: &'a memory::Arena<'_, Internal>,
    ) -> Result<(), &'static str> {
        if self.running {
            return Err("already running");
        }
        if !network
            .borrow()
            .services
            .is_some_and(|s| s.borrow().photo_available())
            || network
                .borrow()
                .services
                .is_some_and(|s| s.borrow().alarm_active())
        {
            return Err("alarm active or display unavailable");
        }
        if self.cache.is_none() {
            let bytes = external
                .allocate_paced_with(Layout::from_size_align(WORKSPACE, 64).unwrap(), || {
                    let _ = network.borrow_mut().io_tick();
                })
                .map_err(|_| "PSRAM workspace exhausted; release caches")?;
            let descriptors = match self.spare_descriptors.take() {
                Some(buffer) => buffer,
                None => working
                    .allocate(Layout::from_size_align(256, 64).unwrap())
                    .map_err(|_| "DMA workspace exhausted")?,
            };
            self.cache = Some(Cache { bytes, descriptors });
        }
        self.count = 0;
        self.folder = http::Text::new();
        write!(
            self.folder,
            "{}",
            if path == "/" {
                "/"
            } else {
                path.trim_end_matches('/')
            }
        )
        .map_err(|_| "path too long")?;
        let mut dir = esp32_p4::runtime::run(fs.read_dir(path), || network.borrow_mut().io_tick())
            .map_err(|_| "network")?
            .map_err(|_| "folder unavailable")?;
        while let Some(entry) =
            esp32_p4::runtime::run(dir.next(), || network.borrow_mut().io_tick())
                .map_err(|_| "network")?
        {
            let entry = entry.map_err(|_| "directory read")?;
            if entry.kind != Kind::File || !is_jpeg(entry.name()) {
                continue;
            }
            if self.count == MAX_FILES {
                return Err("folder exceeds 64 JPEG files");
            }
            let mut full = http::Text::<PATH_BYTES>::new();
            write!(
                full,
                "{}/{}",
                core::str::from_utf8(self.folder.data())
                    .unwrap()
                    .trim_end_matches('/'),
                entry.name()
            )
            .map_err(|_| "path too long")?;
            let len = full.data().len();
            let at = self.count * RECORD;
            let b = self.cache.as_mut().unwrap().bytes.as_mut_slice();
            b[at..at + 2].copy_from_slice(&(len as u16).to_le_bytes());
            b[at + 2..at + 2 + len].copy_from_slice(full.data());
            self.count += 1;
        }
        if self.count == 0 {
            return Err("folder has no JPEG files");
        }
        if cancelled(network) {
            return Err("alarm or server stop");
        }
        let now = network.borrow().port.borrow_mut().now_us();
        self.options = options;
        self.index = 0;
        self.file = http::Text::new();
        self.shown = 0;
        self.skipped = 0;
        self.failed = 0;
        self.next = now;
        self.front = 0;
        self.deadline = if options.seconds == 0 {
            u64::MAX
        } else {
            now + u64::from(options.seconds) * 1_000_000
        };
        self.transition = None;
        self.message = "loading";
        self.running = true;
        network
            .borrow()
            .services
            .unwrap()
            .borrow_mut()
            .photo_mode(true);
        Ok(())
    }
    pub fn stop<P: Port>(
        &mut self,
        network: &RefCell<Network<'a, '_, '_, P>>,
        _panel: &mut esp32_p4::devices::jd9165::Display,
        reason: &'static str,
    ) {
        self.has_picture = false;
        if self.running {
            let services = network.borrow().services;
            if let Some(s) = services {
                // Hide clock glyphs while restoring the permanent framebuffer.
                // Poll between rows so an active alarm keeps refilling I2S.
                s.borrow_mut().photo_mode(true);
                let row = [0x87e0u16; 1024];
                for y in 0..600 {
                    if s.borrow_mut().photo_row(y, &row).is_err() {
                        break;
                    }
                    if y % 8 == 0 {
                        let _ = network.borrow_mut().io_tick();
                    }
                }
                s.borrow_mut().photo_mode(false);
            }
            let lcd = _panel.framebuffer_status();
            let _ = writeln!(network.borrow_mut().console, "PHOTO IDLE LCD {:08x?}", lcd);
            let _ = writeln!(
                network.borrow_mut().console,
                "PHOTO STOP reason={} shown={} skipped={}",
                reason,
                self.shown,
                self.skipped
            );
            let _ = writeln!(
                network.borrow_mut().console,
                "PHOTO STACK {:?}",
                hal::memory::stack_usage()
            );
        }
        self.running = false;
        self.transition = None;
        self.message = reason;
    }
    pub fn release(&mut self) {
        assert!(!self.has_picture);
        if let Some(cache) = self.cache.take() {
            self.spare_descriptors = Some(cache.descriptors);
        }
    }
    pub fn step<P: Port, D: disk::BlockDevice, const N: usize>(
        &mut self,
        fs: &mut FileTable<'_, '_, D, N>,
        network: &RefCell<Network<'a, '_, '_, P>>,
        panel: &mut esp32_p4::devices::jd9165::Display,
        scratch: &mut [u8],
    ) {
        if !self.running {
            return;
        }
        let now = network.borrow().port.borrow_mut().now_us();
        if cancelled(network) {
            self.stop(network, panel, "alarm or server stop");
            return;
        }
        if now >= self.deadline {
            self.stop(network, panel, "time elapsed");
            return;
        }
        if now < self.next {
            return;
        }
        if let Some((back, step, slide)) = self.transition {
            let result = self.transition_frame(network, panel, back, step, slide);
            if let Err(e) = result {
                self.stop(network, panel, e);
                return;
            }
            if step == 6 {
                self.front = back;
                self.transition = None;
                self.shown += 1;
                self.message = "displaying";
                self.next = network.borrow().port.borrow_mut().now_us()
                    + u64::from(self.options.interval) * 1_000_000;
            } else {
                self.transition = Some((back, step + 1, slide));
                self.next = 0;
            }
            return;
        }
        let at = self.index * RECORD;
        let b = self.cache.as_ref().unwrap().bytes.as_slice();
        let len = u16::from_le_bytes([b[at], b[at + 1]]) as usize;
        let mut path = http::Text::<PATH_BYTES>::new();
        let _ = write!(
            path,
            "{}",
            core::str::from_utf8(&b[at + 2..at + 2 + len]).unwrap()
        );
        self.file = path;
        let back = 1 - self.front;
        let begin = network.borrow().port.borrow_mut().now_us();
        let lcd_before = panel.framebuffer_status();
        let result = self.load(fs, network, scratch, back);
        let lcd_after = panel.framebuffer_status();
        let _ = writeln!(
            network.borrow_mut().console,
            "PHOTO LCD prepare before={:x?} after={:x?}",
            lcd_before,
            lcd_after
        );
        self.index = (self.index + 1) % self.count;
        if cancelled(network) {
            self.stop(network, panel, "alarm or server stop");
            return;
        }
        if let Err(e) = result {
            self.failed += 1;
            self.skipped += 1;
            self.message = e;
            self.next = begin + 1_000_000;
            let _ = writeln!(
                network.borrow_mut().console,
                "PHOTO SKIP {} {}",
                core::str::from_utf8(self.file.data()).unwrap(),
                e
            );
            if self.failed >= self.count {
                self.stop(network, panel, "no supported JPEG files");
            }
            return;
        }
        if cancelled(network) {
            self.stop(network, panel, "alarm or server stop");
            return;
        }
        self.failed = 0;
        let elapsed = network.borrow().port.borrow_mut().now_us() - begin;
        let _ = writeln!(
            network.borrow_mut().console,
            "PHOTO READY {} load_us={}",
            core::str::from_utf8(self.file.data()).unwrap(),
            elapsed
        );
        if !self.has_picture {
            if self.present(network, panel, back).is_err() {
                self.stop(network, panel, "display update failed");
                return;
            }
            self.has_picture = true;
        } else if self.options.effects & 12 != 0 {
            let slide = self.options.effects & 8 != 0
                && (self.options.effects & 4 == 0 || self.index & 1 != 0);
            self.transition = Some((back, 1, slide));
            self.message = "transition";
            self.next = 0;
            return;
        } else if self.present(network, panel, back).is_err() {
            self.stop(network, panel, "display update failed");
            return;
        }
        self.front = back;
        self.shown += 1;
        self.message = "displaying";
        self.next = network.borrow().port.borrow_mut().now_us()
            + u64::from(self.options.interval) * 1_000_000;
    }
    fn present<P: Port>(
        &self,
        network: &RefCell<Network<'a, '_, '_, P>>,
        _panel: &mut esp32_p4::devices::jd9165::Display,
        index: usize,
    ) -> Result<(), &'static str> {
        let started = network.borrow().port.borrow_mut().now_us();
        let source = &self.cache.as_ref().unwrap().bytes.as_slice()
            [FRAMES_AT + index * FRAME_SPAN + GUARD..][..framebuffer::BYTES];
        let services = network.borrow().services.ok_or("display unavailable")?;
        let mut row = [0u16; 1024];
        for y in 0..600 {
            for x in 0..512 {
                // Aligned uncached PSRAM word loads instead of four byte loads.
                let word = unsafe {
                    source
                        .as_ptr()
                        .add(y * 2048 + x * 4)
                        .cast::<u32>()
                        .read_volatile()
                };
                row[2 * x] = word as u16;
                row[2 * x + 1] = (word >> 16) as u16;
            }
            services.borrow_mut().photo_row(y, &row)?;
            if y % 8 == 0 {
                network.borrow_mut().io_tick().map_err(|_| "network")?;
                if cancelled(network) {
                    return Err("alarm or server stop");
                }
            }
        }
        let elapsed = network.borrow().port.borrow_mut().now_us() - started;
        let _ = writeln!(network.borrow_mut().console, "PHOTO PRESENT us={}", elapsed);
        Ok(())
    }
    fn load<P: Port, D: disk::BlockDevice, const N: usize>(
        &mut self,
        fs: &mut FileTable<'_, '_, D, N>,
        network: &RefCell<Network<'a, '_, '_, P>>,
        scratch: &mut [u8],
        back: usize,
    ) -> Result<(), &'static str> {
        let path = core::str::from_utf8(self.file.data()).unwrap();
        let handle = esp32_p4::runtime::run(fs.open(path, OpenMode::Read), || {
            network.borrow_mut().io_tick()
        })
        .map_err(|_| "network")?
        .map_err(|_| "file missing or busy")?;
        let result = (|| {
            let len = fs.info(handle).map_err(|_| "file info")?.len as usize;
            if len == 0 || len > INPUT {
                return Err("JPEG exceeds 2MiB input limit");
            }
            let cache = self.cache.as_mut().unwrap();
            let (meta, input_tail) = cache.bytes.as_mut_slice().split_at_mut(INPUT_AT);
            meta[META..].fill(0xa5);
            let (input, tail) = input_tail.split_at_mut(INPUT + 2 * GUARD);
            let (decoded, tail) = tail.split_at_mut(DECODED + 2 * GUARD);
            let span = &mut tail[back * FRAME_SPAN..(back + 1) * FRAME_SPAN];
            span[..GUARD].fill(0x77);
            span[GUARD + framebuffer::BYTES..].fill(0x77);
            let (frame_before, visible) = span.split_at_mut(GUARD);
            let (frame, frame_guard) = visible.split_at_mut(framebuffer::BYTES);
            input[INPUT..].fill(0xa5);
            decoded[DECODED..].fill(0x5a);
            cache.descriptors.as_mut_slice()[..64].fill(0x3c);
            cache.descriptors.as_mut_slice()[192..].fill(0x3c);
            let mut at = 0;
            while at < len {
                let want = scratch.len().min(len - at);
                let n = esp32_p4::runtime::run(fs.read(handle, &mut scratch[..want]), || {
                    network.borrow_mut().io_tick()
                })
                .map_err(|_| "network")?
                .map_err(|_| "SD read")?;
                if n == 0 {
                    return Err("truncated JPEG");
                }
                web::upload::copy_paced(&mut input[at..at + n], &scratch[..n]);
                at += n;
                if cancelled(network) {
                    return Err("alarm or server stop");
                }
            }
            let h =
                hal::jpeg::Header::parse(&input[..len]).map_err(|_| "unsupported JPEG header")?;
            let bytes = usize::from(h.stride) * usize::from(h.rows) * 2;
            if bytes > DECODED {
                return Err("decoded image exceeds 8MiB limit");
            }
            let mut cfg = self
                .options
                .layout(h.width, h.height, self.shown as usize)
                .ok_or("unsupported SRM geometry")?;
            cfg.source_size = (h.stride, h.rows);
            let result = network
                .borrow()
                .port
                .borrow_mut()
                .with_timer(|timer| unsafe {
                    hal::jpeg::decode(
                        &input[..len],
                        &mut decoded[..h.output_bytes()],
                        &h,
                        &mut cache.descriptors.as_mut_slice()[64..192],
                        hal::jpeg::ColorConversion::JpegFull,
                        timer,
                    )
                });
            if let Err(e) = result {
                let _ = writeln!(network.borrow_mut().console, "PHOTO JPEG {:x?}", e);
                return Err("JPEG hardware error");
            }
            if cancelled(network) {
                return Err("alarm or server stop");
            }
            if h.components == 1 {
                for i in (0..bytes / 2).rev() {
                    let g = u16::from(decoded[i]);
                    let pixel = ((g >> 3) << 11) | ((g >> 2) << 5) | (g >> 3);
                    decoded[2 * i..2 * i + 2].copy_from_slice(&pixel.to_le_bytes());
                    if i.is_multiple_of(2048) {
                        network.borrow_mut().io_tick().map_err(|_| "network")?;
                    }
                }
            }
            // Clear only the inactive surface, yielding between bounded bursts.
            for chunk in frame.chunks_mut(4096) {
                web::upload::copy_paced(chunk, &[0; 4096][..chunk.len()]);
                network.borrow_mut().io_tick().map_err(|_| "network")?;
                if cancelled(network) {
                    return Err("alarm or server stop");
                }
            }
            let result = network
                .borrow()
                .port
                .borrow_mut()
                .with_timer(|timer| unsafe {
                    hal::ppa::srm_rgb565(
                        &decoded[..bytes],
                        frame,
                        cfg,
                        &mut cache.descriptors.as_mut_slice()[64..192],
                        timer,
                    )
                });
            if let Err(e) = result {
                let _ = writeln!(network.borrow_mut().console, "PHOTO SRM {:x?}", e);
                return Err("SRM hardware error");
            }
            if frame_before.iter().any(|b| *b != 0x77)
                || meta[META..].iter().any(|b| *b != 0xa5)
                || frame_guard.iter().any(|b| *b != 0x77)
                || input[INPUT..].iter().any(|b| *b != 0xa5)
                || decoded[DECODED..].iter().any(|b| *b != 0x5a)
                || cache.descriptors.as_slice()[..64]
                    .iter()
                    .chain(cache.descriptors.as_slice()[192..].iter())
                    .any(|b| *b != 0x3c)
            {
                return Err("DMA guard damaged");
            }
            Ok(())
        })();
        let close = esp32_p4::runtime::run(fs.close(handle), || network.borrow_mut().io_tick())
            .map_err(|_| "network")?
            .map_err(|_| "file close");
        result.and(close)
    }
    fn transition_frame<P: Port>(
        &mut self,
        network: &RefCell<Network<'a, '_, '_, P>>,
        panel: &mut esp32_p4::devices::jd9165::Display,
        back: usize,
        step: u8,
        slide: bool,
    ) -> Result<(), &'static str> {
        if step == 6 {
            return self.present(network, panel, back);
        }
        let stage = 2 + usize::from(step & 1);
        let b = self.cache.as_mut().unwrap().bytes.as_mut_slice();
        let (images, stages) = b[FRAMES_AT..].split_at_mut(2 * FRAME_SPAN);
        let old = &images
            [self.front * FRAME_SPAN + GUARD..self.front * FRAME_SPAN + GUARD + framebuffer::BYTES];
        let new =
            &images[back * FRAME_SPAN + GUARD..back * FRAME_SPAN + GUARD + framebuffer::BYTES];
        let out = &mut stages[(stage - 2) * FRAME_SPAN + GUARD
            ..(stage - 2) * FRAME_SPAN + GUARD + framebuffer::BYTES];
        let cut = ((usize::from(step) * 1024 / 6) & !1) * 2;
        for y in 0..600 {
            let at = y * 2048;
            let row = &mut out[at..at + 2048];
            if slide {
                web::upload::copy_paced(&mut row[..2048 - cut], &old[at + cut..at + 2048]);
                web::upload::copy_paced(&mut row[2048 - cut..], &new[at..at + cut]);
            } else {
                web::upload::copy_paced(&mut row[..cut], &new[at..at + cut]);
                web::upload::copy_paced(&mut row[cut..], &old[at + cut..at + 2048]);
            }
            if y % 8 == 0 {
                network.borrow_mut().io_tick().map_err(|_| "network")?;
                if cancelled(network) {
                    return Err("alarm or server stop");
                }
            }
        }
        self.present(network, panel, stage)
    }
}
fn cancelled<P: Port>(network: &RefCell<Network<'_, '_, '_, P>>) -> bool {
    let n = network.borrow();
    n.stop
        || n.services.is_some_and(|s| s.borrow().alarm_active())
        || n.port.borrow_mut().now_us() >= n.deadline
}
