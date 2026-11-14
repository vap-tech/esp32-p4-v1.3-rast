//! Board integration for the bounded TCP/HTTP file server. Single cooperative
//! task owns SD+EMAC. RefCell serializes their access to the sole SYSTIMER handle.
mod files;
#[cfg(feature = "diagnostics")]
mod storage_lab;
use core::{
    cell::{Cell, RefCell},
    fmt::Write,
};
use esp32_p4::{
    hal, memory,
    net::{self, http, tcp},
    storage::{
        asynchronous::{self as disk},
        files::FileTable,
        volume::Volume,
    },
    web::{self, Asset, Transport},
};
const HTTP_SCRATCH: usize = match option_env!("P4_HTTP_SCRATCH_KIB") {
    Some(v) if v.as_bytes()[0] == b'3' => 32768,
    Some(v) if v.as_bytes()[0] == b'1' => 16384,
    _ => 8192,
};
/// One owner serializes controller I/O and lends its sole timer to SD.
pub(crate) trait Port {
    #[cfg(feature = "wifi")]
    fn diagnostics(&self) -> Option<crate::wifi::Stats> {
        None
    }

    #[cfg(feature = "diagnostics")]
    fn take_rx_losses(&mut self) -> u32 {
        0
    }
    fn wired_stats(&self) -> Option<[u32; 6]> {
        None
    }
    fn reset_stats(&mut self) {}
    fn shares_sdmmc(&self) -> bool {
        false
    }
    fn with_timer<R>(&mut self, operation: impl FnOnce(&mut hal::time::Timer) -> R) -> R;
    fn now_us(&mut self) -> u64 {
        self.with_timer(|t| t.now_us())
    }
    fn address(&self) -> [u8; 6];
    fn receive(&mut self, out: &mut [u8]) -> Result<Option<usize>, web::Error>;
    fn transmit(&mut self, bytes: &[u8]) -> Result<(), web::Error>;
    fn status(&self) -> [u32; 6];
}
impl Port for hal::ethernet::Emac<'_, '_> {
    #[cfg(feature = "diagnostics")]
    fn take_rx_losses(&mut self) -> u32 {
        hal::ethernet::Emac::take_rx_losses(self)
    }
    fn wired_stats(&self) -> Option<[u32; 6]> {
        Some(self.stats())
    }
    fn reset_stats(&mut self) {
        hal::ethernet::Emac::reset_stats(self);
    }

    fn with_timer<R>(&mut self, operation: impl FnOnce(&mut hal::time::Timer) -> R) -> R {
        hal::ethernet::Emac::with_timer(self, operation)
    }
    fn address(&self) -> [u8; 6] {
        net::MAC
    }
    fn receive(&mut self, out: &mut [u8]) -> Result<Option<usize>, web::Error> {
        hal::ethernet::Emac::receive(self, out.try_into().map_err(|_| web::Error::Frame)?).map_err(
            |e| match e {
                hal::ethernet::Error::Frame(_) => web::Error::Frame,
                _ => web::Error::Transport,
            },
        )
    }
    fn transmit(&mut self, bytes: &[u8]) -> Result<(), web::Error> {
        hal::ethernet::Emac::transmit(self, bytes).map_err(|_| web::Error::Transport)
    }
    fn status(&self) -> [u32; 6] {
        hal::ethernet::Emac::status(self)
    }
}
static mut SOCKETS: [tcp::Storage<'static>; tcp::SLOTS] =
    [const { tcp::Storage::EMPTY }; tcp::SLOTS];
static mut CLIPBOARD: http::Text<1024> = http::Text::new();
#[derive(Clone, Copy, Debug, Default)]
struct WriteStats {
    direct_bytes: u64,
    direct_requests: u32,
    bounce_bytes: u64,
}
struct Card<'a, P: Port> {
    card: hal::sdmmc::Sdmmc,
    port: &'a RefCell<P>,
    busy: &'a Cell<bool>,
    direct_psram: bool,
    writes: &'a Cell<WriteStats>,
}
struct Busy<'a>(&'a Cell<bool>);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}
impl<P: Port> disk::BlockDevice for Card<'_, P> {
    type Error = esp32_p4::storage::sd::Error;
    fn sectors(&self) -> u64 {
        self.card.info.sectors
    }
    fn max_transfer_bytes(&self) -> usize {
        hal::sdmmc::MAX_DIRECT_TRANSFER
    }
    async fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), Self::Error> {
        for (index, part) in out.chunks_mut(hal::sdmmc::MAX_TRANSFER).enumerate() {
            esp32_p4::runtime::yield_now().await;
            let address = lba + (index * hal::sdmmc::MAX_TRANSFER / 512) as u64;
            let mut request = self
                .card
                .request_read(
                    u32::try_from(address).map_err(|_| Self::Error::AddressOverflow)?,
                    part.len(),
                )
                .map_err(Self::Error::Card)?;
            assert!(!self.busy.replace(true));
            let _busy = Busy(self.busy);
            core::future::poll_fn(|cx| {
                self.port
                    .borrow_mut()
                    .with_timer(|timer| self.card.poll_request(&mut request, cx, timer, part, None))
            })
            .await
            .map_err(Self::Error::Card)?;
        }
        Ok(())
    }
    async fn write(&mut self, lba: u64, bytes: &[u8]) -> Result<(), Self::Error> {
        #[allow(unused_mut)]
        let mut direct = self.direct_psram
            && bytes.as_ptr() as usize >= 0x88000000
            && (bytes.as_ptr() as usize)
                .checked_add(bytes.len())
                .is_some_and(|end| end <= 0x8a000000)
            && (bytes.as_ptr() as usize).is_multiple_of(64)
            && bytes.len().is_multiple_of(512);
        #[cfg(feature = "sd-wide-bench")]
        {
            direct |= hal::sdmmc::profile::DIRECT_TX.load(core::sync::atomic::Ordering::Relaxed)
                && bytes.len() == 32768;
        }
        let chunk = if direct {
            hal::sdmmc::MAX_DIRECT_TRANSFER
        } else {
            hal::sdmmc::MAX_TRANSFER
        };
        for (index, part) in bytes.chunks(chunk).enumerate() {
            esp32_p4::runtime::yield_now().await;
            let address = u32::try_from(lba + (index * chunk / 512) as u64)
                .map_err(|_| Self::Error::AddressOverflow)?;
            let mut request = if direct {
                // SAFETY: this future holds the immutable source borrow across all
                // polls; Request::Drop quiesces DMA before that borrow is released.
                unsafe { self.card.request_write_direct(address, part) }
            } else {
                self.card.request_write(address, part.len())
            }
            .map_err(Self::Error::Card)?;
            assert!(!self.busy.replace(true));
            let _busy = Busy(self.busy);
            core::future::poll_fn(|cx| {
                self.port.borrow_mut().with_timer(|timer| {
                    self.card
                        .poll_request(&mut request, cx, timer, &mut [], Some(part))
                })
            })
            .await
            .map_err(Self::Error::Card)?;
            let mut stats = self.writes.get();
            if direct {
                stats.direct_bytes += part.len() as u64;
                stats.direct_requests += 1;
            } else {
                stats.bounce_bytes += part.len() as u64;
            }
            self.writes.set(stats);
        }
        Ok(())
    }
    async fn sync(&mut self) -> Result<(), Self::Error> {
        Ok(())
    } // write awaits CMD13 READY/TRANSFER
}
struct FlashPage {
    length: u32,
}
impl FlashPage {
    fn open() -> Result<Self, web::Error> {
        let mut h = [0u32; 4];
        unsafe { hal::flash::read(0x100000, &mut h) }.map_err(|_| web::Error::Asset)?;
        if h[0] != u32::from_le_bytes(*b"P4WE")
            || h[1] != u32::from_le_bytes(*b"B001")
            || h[2] == 0
            || h[2] > 65536
        {
            return Err(web::Error::Asset);
        }
        let mut page = Self { length: h[2] };
        let mut offset = 0;
        let mut crc = !0u32;
        let mut buf = [0u8; 1024];
        while offset < page.length {
            let n = page.read(offset, &mut buf)?;
            for &b in &buf[..n] {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb88320 } else { 0 };
                }
            }
            offset += n as u32;
        }
        if !crc != h[3] {
            return Err(web::Error::Asset);
        }
        Ok(page)
    }
}
impl Asset for FlashPage {
    fn len(&self) -> u32 {
        self.length
    }
    fn read(&mut self, offset: u32, out: &mut [u8]) -> Result<usize, web::Error> {
        // All nonfinal chunks are word-sized. Small workspace stays in SRAM.
        if !offset.is_multiple_of(4) || offset > self.length {
            return Err(web::Error::Asset);
        }
        let mut words = [0u32; 256];
        let n = out.len().min(1024).min((self.length - offset) as usize);
        unsafe { hal::flash::read(0x100010 + offset, &mut words[..n.div_ceil(4)]) }
            .map_err(|_| web::Error::Asset)?;
        for (i, b) in out[..n].iter_mut().enumerate() {
            *b = words[i / 4].to_le_bytes()[i % 4];
        }
        Ok(n)
    }
}
/// TX tokens lend a CPU workspace until the driver has copied the frame into
/// its owned DMA storage; hardware completion may happen after the token returns.
struct NetDevice<'a, P: Port> {
    port: &'a RefCell<P>,
    rx: &'a mut [u8; net::FRAME],
    tx: &'a mut [u8; net::FRAME],
    error: &'a mut Option<web::Error>,
    rx_errors: &'a mut u32,
    busy: &'a Cell<bool>,
    shared: bool,
    rx_budget: usize,
    tx_budget: usize,
}
struct NetRx<'a>(&'a [u8]);
struct NetTx<'a, P: Port> {
    port: &'a RefCell<P>,
    buffer: &'a mut [u8; net::FRAME],
    error: &'a mut Option<web::Error>,
}
impl smoltcp::phy::RxToken for NetRx<'_> {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(self.0)
    }
}
impl<P: Port> smoltcp::phy::TxToken for NetTx<'_, P> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let result = f(&mut self.buffer[..len]);
        if let Err(error) = self.port.borrow_mut().transmit(&self.buffer[..len]) {
            *self.error = Some(error);
        }
        result
    }
}
impl<P: Port> smoltcp::phy::Device for NetDevice<'_, P> {
    type RxToken<'b>
        = NetRx<'b>
    where
        Self: 'b;
    type TxToken<'b>
        = NetTx<'b, P>
    where
        Self: 'b;
    fn capabilities(&self) -> smoltcp::phy::DeviceCapabilities {
        tcp::capabilities()
    }
    fn receive(
        &mut self,
        _: smoltcp::time::Instant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.error.is_some()
            || self.rx_budget == 0
            || self.tx_budget == 0
            || self.shared && self.busy.get()
        {
            return None;
        }
        self.rx_budget -= 1;
        let received = self.port.borrow_mut().receive(self.rx);
        match received {
            Ok(Some(n)) => {
                self.tx_budget -= 1;
                Some((
                    NetRx(&self.rx[..n]),
                    NetTx {
                        port: self.port,
                        buffer: self.tx,
                        error: self.error,
                    },
                ))
            }
            Ok(None) => None,
            Err(web::Error::Frame) => {
                *self.rx_errors += 1;
                None
            }
            Err(error) => {
                *self.error = Some(error);
                None
            }
        }
    }
    fn transmit(&mut self, _: smoltcp::time::Instant) -> Option<Self::TxToken<'_>> {
        if self.error.is_some() || self.tx_budget == 0 || self.shared && self.busy.get() {
            return None;
        }
        self.tx_budget -= 1;
        Some(NetTx {
            port: self.port,
            buffer: self.tx,
            error: self.error,
        })
    }
}
struct Network<'a, 'c, P: Port> {
    port: &'a RefCell<P>,
    tcp: tcp::Stack<'a>,
    console: &'c mut hal::usb_serial::Console,
    rx: [u8; net::FRAME],
    tx: [u8; net::FRAME],
    ip: [u8; 4],
    deadline: u64,
    stop: bool,
    rx_errors: u32,
    #[cfg(feature = "diagnostics")]
    losses: [u64; 4],
    #[cfg(feature = "diagnostics")]
    last_tick: u64,
    #[cfg(feature = "diagnostics")]
    max_tick_gap: u64,
    ticks: u64,
    poll_us: u64,
    read_empty: u64,
    write_full: u64,
    writes: &'a Cell<WriteStats>,
    busy: &'a Cell<bool>,
}
impl<P: Port> Network<'_, '_, P> {
    #[cfg(feature = "diagnostics")]
    fn sample_losses(&mut self) -> [u64; 4] {
        let raw = self.port.borrow_mut().take_rx_losses();
        self.losses[0] += u64::from(raw & 0xffff);
        self.losses[1] += u64::from((raw >> 17) & 0x7ff);
        self.losses[2] += u64::from((raw >> 16) & 1);
        self.losses[3] += u64::from((raw >> 28) & 1);
        self.losses
    }
    // Complete the current bounded FS operation before honoring ESC/deadline.
    // Cancelling its DMA halfway through would quarantine the shared controller.
    fn io_tick(&mut self) -> Result<(), web::Error> {
        match self.tick() {
            Err(_) if self.stop || self.port.borrow_mut().now_us() >= self.deadline => Ok(()),
            result => result,
        }
    }
    #[inline(never)]
    fn tick(&mut self) -> Result<(), web::Error> {
        if self.console.read_byte() == Some(27) {
            self.stop = true;
        }
        let now = self.port.borrow_mut().now_us();
        if self.stop || now >= self.deadline {
            return Err(web::Error::Transport);
        }
        #[cfg(feature = "diagnostics")]
        {
            if self.last_tick != 0 {
                self.max_tick_gap = self.max_tick_gap.max(now - self.last_tick);
            }
            self.last_tick = now;
        }
        let mut error = None;
        let mut device = NetDevice {
            port: self.port,
            rx: &mut self.rx,
            tx: &mut self.tx,
            error: &mut error,
            rx_errors: &mut self.rx_errors,
            busy: self.busy,
            shared: self.port.borrow().shares_sdmmc(),
            rx_budget: 16,
            tx_budget: 16,
        };
        self.tcp.poll(now, &mut device);
        self.ticks += 1;
        #[cfg(feature = "diagnostics")]
        if self.ticks.is_multiple_of(256) {
            self.sample_losses();
        }
        self.poll_us += self.port.borrow_mut().now_us() - now;
        if let Some(error) = error {
            return Err(error);
        }

        Ok(())
    }
}
struct Connection<'a, 'n, 'c, P: Port> {
    network: &'a RefCell<Network<'n, 'c, P>>,
    slot: usize,
    buffer: [u8; tcp::MSS],
    used: usize,
}
impl<P: Port> Connection<'_, '_, '_, P> {
    fn now(&self) -> u64 {
        self.network.borrow().port.borrow_mut().now_us()
    }
    fn tick(&self) -> Result<(), web::Error> {
        self.network.borrow_mut().tick()
    }
    fn finish(&mut self) -> Result<(), web::Error> {
        self.flush()?;
        let start = self.now();
        while !self.network.borrow().tcp.flushed(self.slot) {
            if !self.network.borrow().tcp.alive(self.slot) || self.now() - start > 30_000_000 {
                return Err(web::Error::Transport);
            }
            self.tick()?;
        }
        Ok(())
    }
}
impl<P: Port> Transport for Connection<'_, '_, '_, P> {
    fn read(&mut self, out: &mut [u8]) -> Result<usize, web::Error> {
        let start = self.now();
        loop {
            if self.now() - start > 30_000_000 {
                return Err(web::Error::Transport);
            }
            let n = self.network.borrow_mut().tcp.read(self.slot, out);
            if n != 0 {
                return Ok(n);
            }
            if self.network.borrow().tcp.eof(self.slot) {
                return Ok(0);
            }
            self.network.borrow_mut().read_empty += 1;
            self.tick()?;
        }
    }
    fn write_all(&mut self, mut bytes: &[u8]) -> Result<(), web::Error> {
        while !bytes.is_empty() {
            let n = bytes.len().min(self.buffer.len() - self.used);
            self.buffer[self.used..self.used + n].copy_from_slice(&bytes[..n]);
            self.used += n;
            bytes = &bytes[n..];
            if self.used == self.buffer.len() {
                self.flush()?;
            }
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), web::Error> {
        let mut at = 0;
        let start = self.now();
        while at < self.used {
            if self.now() - start > 30_000_000 || !self.network.borrow().tcp.alive(self.slot) {
                return Err(web::Error::Transport);
            }
            let before = at;
            at += self
                .network
                .borrow_mut()
                .tcp
                .write(self.slot, &self.buffer[at..self.used]);
            if at == before {
                self.network.borrow_mut().write_full += 1;
            }
            self.tick()?;
        }
        self.used = 0;
        self.tick()
    }
}
#[inline(never)]
pub fn run(
    seconds: u32,
    arena: &memory::Arena<'_, memory::Internal>,
    working: &memory::Arena<'_, memory::Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    panel: &mut esp32_p4::devices::jd9165::Display,
    external: &mut memory::Arena<'_, memory::External>,
) -> Result<(), web::Error> {
    if external.stats().used != 0 {
        return Err(web::Error::Storage);
    }
    let result = run_mode(
        seconds,
        arena,
        timer,
        console,
        panel,
        Resources {
            working,
            external: Some(external),
            benchmark: None,
        },
    );
    external.reset();
    result
}
struct Resources<'a, 'mem> {
    working: &'a memory::Arena<'mem, memory::Internal>,
    external: Option<&'a memory::Arena<'mem, memory::External>>,
    benchmark: Option<StorageBench<'a>>,
}
struct StorageBench<'a> {
    #[cfg(feature = "diagnostics")]
    mib: u32,
    #[cfg(feature = "sd-wide-bench")]
    psram: &'a mut [u8],
    _lifetime: core::marker::PhantomData<&'a mut ()>,
}
#[cfg(feature = "diagnostics")]
pub fn run_storage_bench(
    mib: u32,
    arena: &memory::Arena<'_, memory::Internal>,
    working: &memory::Arena<'_, memory::Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    panel: &mut esp32_p4::devices::jd9165::Display,
    #[cfg(feature = "sd-wide-bench")] external: &mut memory::Arena<'_, memory::External>,
) -> Result<(), web::Error> {
    #[cfg(feature = "sd-wide-bench")]
    if external.stats().used != 0 {
        return Err(web::Error::Storage);
    }
    let result = {
        #[cfg(feature = "sd-wide-bench")]
        let mut buffer = external
            .allocate(core::alloc::Layout::from_size_align(32768 + 128, 64).unwrap())
            .map_err(|_| web::Error::Storage)?;
        run_mode(
            600,
            arena,
            timer,
            console,
            panel,
            Resources {
                working,
                external: None,
                benchmark: Some(StorageBench {
                    mib,
                    #[cfg(feature = "sd-wide-bench")]
                    psram: buffer.as_mut_slice(),
                    _lifetime: core::marker::PhantomData,
                }),
            },
        )
    };
    #[cfg(feature = "sd-wide-bench")]
    external.reset();
    result
}
#[inline(never)]
fn run_mode(
    seconds: u32,
    arena: &memory::Arena<'_, memory::Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    panel: &mut esp32_p4::devices::jd9165::Display,
    resources: Resources<'_, '_>,
) -> Result<(), web::Error> {
    let cache = hal::memory::cache_snapshot();
    if cache.l1_bypass & 16 == 0 || cache.l2_size != 0 {
        return Err(web::Error::CacheConfiguration);
    }
    // Instruction caching is independent of uncached data/DMA ownership.
    unsafe { hal::memory::set_instruction_cache(true) }
        .map_err(|_| web::Error::CacheConfiguration)?;
    let _ = writeln!(console, "WEB ICACHE ON (D-cache remains bypassed)");
    let mut mdio = unsafe { hal::ethernet::Mdio::new() };
    let link = (|| {
        let _ = mdio.read(1, 1, timer)?;
        let status = mdio.read(1, 1, timer)?;
        let common = mdio.read(1, 4, timer)? & mdio.read(1, 5, timer)?;
        Ok::<_, hal::ethernet::Error>(status & 0x24 == 0x24 && common & 0x100 != 0)
    })()
    .map_err(|_| web::Error::Link)?;
    if !link {
        return Err(web::Error::Link);
    }

    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(|_| web::Error::Storage)?;
    card.configure_bus(timer).map_err(|_| web::Error::Storage)?;
    let port = RefCell::new(
        unsafe { hal::ethernet::Emac::start(arena, timer, net::MAC) }
            .map_err(|_| web::Error::Transport)?,
    );
    serve(&port, card, arena, seconds, console, panel, resources)
}
#[cfg(feature = "wifi")]
pub fn run_wifi(
    seconds: u32,
    arena: &memory::Arena<'_, memory::Internal>,
    working: &memory::Arena<'_, memory::Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    panel: &mut esp32_p4::devices::jd9165::Display,
) -> Result<(), web::Error> {
    let cache = hal::memory::cache_snapshot();
    if cache.l1_bypass & 16 == 0 || cache.l2_size != 0 {
        return Err(web::Error::CacheConfiguration);
    }
    // Initialize memory card first. C6 init resets host logic, not the SD card;
    // the idle card handle restores its slot settings on its first command.
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(|_| web::Error::Storage)?;
    card.configure_bus(timer).map_err(|_| web::Error::Storage)?;
    let mut detail = None;
    let outcome =
        crate::wifi::with_default_link(timer, console, |link, address, timer, console| {
            let port = RefCell::new(crate::wifi::WifiPort::new(link, address, timer));
            match serve(
                &port,
                card,
                arena,
                seconds,
                console,
                panel,
                Resources {
                    working,
                    external: None,
                    benchmark: None,
                },
            ) {
                Ok(()) => Ok(()),
                Err(e) => {
                    detail = Some(e);
                    Err("web service")
                }
            }
        });
    if let Some(error) = detail {
        return Err(error);
    }
    outcome.map_err(|_| web::Error::Link)
}
#[inline(never)]
fn serve<P: Port>(
    port: &RefCell<P>,
    card: hal::sdmmc::Sdmmc,
    arena: &memory::Arena<'_, memory::Internal>,
    seconds: u32,
    console: &mut hal::usb_serial::Console,
    panel: &mut esp32_p4::devices::jd9165::Display,
    resources: Resources<'_, '_>,
) -> Result<(), web::Error> {
    let Resources {
        working,
        external,
        benchmark: benchmark_mib,
    } = resources;
    #[cfg(not(feature = "diagnostics"))]
    let _ = benchmark_mib;
    let mut asset = FlashPage::open()?;
    let xid = port.borrow_mut().now_us() as u32;
    let busy = Cell::new(false);
    let writes = Cell::new(WriteStats::default());
    // Queue storage is a scoped SRAM lease; 88KiB is reclaimed when web-run ends.
    let mut allocation = arena
        .allocate(
            core::alloc::Layout::from_size_align(core::mem::size_of::<tcp::Buffers>(), 64).unwrap(),
        )
        .map_err(|_| web::Error::Storage)?;
    let buffers = unsafe {
        &mut *allocation
            .as_mut_slice()
            .as_mut_ptr()
            .cast::<tcp::Buffers>()
    };
    let address = port.borrow().address();
    let tcp = unsafe {
        tcp::Stack::new(
            &mut *core::ptr::addr_of_mut!(SOCKETS).cast::<[tcp::Storage<'_>; tcp::SLOTS]>(),
            buffers,
            address,
            port.borrow_mut().now_us(),
        )
    };
    let start = port.borrow_mut().now_us();
    let mut network = Network {
        port,
        tcp,
        console,
        rx: [0; net::FRAME],
        tx: [0; net::FRAME],
        ip: [0; 4],
        deadline: start + u64::from(seconds) * 1_000_000,
        stop: false,
        rx_errors: 0,
        #[cfg(feature = "diagnostics")]
        losses: [0; 4],
        #[cfg(feature = "diagnostics")]
        last_tick: 0,
        #[cfg(feature = "diagnostics")]
        max_tick_gap: 0,
        ticks: 0,
        poll_us: 0,
        read_empty: 0,
        write_full: 0,
        busy: &busy,
        writes: &writes,
    };
    let mut offer = None;
    let mut next = 0;
    let mut lease = None;
    while port.borrow_mut().now_us() < network.deadline {
        let now = port.borrow_mut().now_us();
        if now >= next {
            let n = net::dhcp_for_mac(&mut network.tx, xid, offer, address);
            port.borrow_mut()
                .transmit(&network.tx[..n])
                .map_err(|_| web::Error::Transport)?;
            next = now + 3_000_000;
        }
        if network.console.read_byte() == Some(27) {
            return Ok(());
        }
        let n = port
            .borrow_mut()
            .receive(&mut network.rx)
            .map_err(|_| web::Error::Transport)?;
        if let Some(n) = n {
            if let Some((kind, got)) = net::dhcp_reply_for_mac(&network.rx[..n], xid, address) {
                if kind == 2 && offer.is_none() && got.address != [0; 4] && got.server != [0; 4] {
                    offer = Some(got);
                    next = now;
                } else if kind == 5
                    && got.seconds != 0
                    && offer.is_some_and(|o| o.address == got.address && o.server == got.server)
                {
                    lease = Some(got);
                    break;
                } else if kind == 6 && offer.is_some_and(|o| o.server == got.server) {
                    offer = None;
                    next = now + 3_000_000;
                }
            }
        }
    }
    let lease = lease.ok_or(web::Error::Transport)?;
    network.ip = lease.address;
    network
        .tcp
        .configure(lease.address, lease.mask, lease.router)
        .map_err(|_| web::Error::Transport)?;
    network.deadline = network
        .deadline
        .min(port.borrow_mut().now_us() + u64::from(lease.seconds) * 1_000_000);
    let a = network.ip;
    let network = RefCell::new(network);
    let _irq = unsafe { hal::interrupt::Guard::enter() };
    #[allow(unused_mut)]
    let mut partition = esp32_p4::runtime::run(
        disk::Partition::mbr(
            Card {
                card,
                port,
                busy: &busy,
                direct_psram: external.is_some(),
                writes: &writes,
            },
            0,
        ),
        || network.borrow_mut().tick(),
    )?
    .map_err(|_| web::Error::Storage)?;
    #[cfg(feature = "diagnostics")]
    storage_lab::configure(&mut partition, &network)?;
    let mut stream = RefCell::new(disk::Stream::new(partition));
    let mut volume = esp32_p4::runtime::run(Volume::mount(&mut stream), || {
        network.borrow_mut().io_tick()
    })?
    .map_err(|_| web::Error::Storage)?;
    let mut fs = FileTable::<_, 5>::new(&mut volume);
    let mut scratch_allocation = working
        .allocate(core::alloc::Layout::from_size_align(HTTP_SCRATCH, 64).unwrap())
        .map_err(|_| web::Error::Storage)?;
    let scratch = scratch_allocation.as_mut_slice();
    #[cfg(feature = "diagnostics")]
    {
        if let Some(bench) = benchmark_mib {
            let lcd_before = panel.framebuffer_status();
            let result = storage_lab::run(
                &mut fs,
                port,
                network.borrow_mut().console,
                scratch,
                bench.mib,
                #[cfg(feature = "sd-wide-bench")]
                bench.psram,
            );
            let _ = writeln!(
                network.borrow_mut().console,
                "SD FILE BENCH LCD before={:08x?} after={:08x?}",
                lcd_before,
                panel.framebuffer_status()
            );
            let cleanup = esp32_p4::runtime::run(fs.shutdown(), || Ok::<(), web::Error>(()))?
                .map_err(|_| web::Error::Storage);
            return result.and(cleanup);
        }
        hal::sdmmc::profile::reset();
    }
    let _ = writeln!(
        network.borrow_mut().console,
        "WEB READY http://{}.{}.{}.{}/ seconds={} FS=async-fat handles=5 metadata={}B parallel-files staging={} ESC=stop",
        a[0],
        a[1],
        a[2],
        a[3],
        seconds,
        core::mem::size_of_val(&fs),
        if external.is_some() { "PSRAM-ring" } else { "SRAM" }
    );

    let _ = writeln!(
        network.borrow_mut().console,
        "WEB RING blocks={} block={} capacity={} per-upload sd_bounce={} http_scratch={}",
        web::upload::BLOCKS,
        web::upload::BLOCK,
        web::upload::CHUNK,
        hal::sdmmc::MAX_TRANSFER,
        HTTP_SCRATCH
    );
    let mut staging = web::upload::UploadPool::<{ tcp::SLOTS }>::new(external);
    let mut requests = 0;
    let mut transfers = [files::Transfer::Idle; tcp::SLOTS];
    let mut progress = [port.borrow_mut().now_us(); tcp::SLOTS];
    let mut turn = 0;
    let outcome = 'service: loop {
        #[cfg(feature = "diagnostics")]
        if transfers.iter().filter(|t| !t.idle()).count() > 1 {
            for t in &mut transfers {
                t.mark_shared();
            }
        }
        for offset in 0..tcp::SLOTS {
            let slot = (turn + offset) % tcp::SLOTS;
            if transfers[slot].idle() {
                continue;
            }
            let operation = if port.borrow_mut().now_us() - progress[slot] > 30_000_000 {
                Err(web::Error::Transport)
            } else {
                esp32_p4::runtime::run(
                    files::step(
                        &mut transfers[slot],
                        slot,
                        &mut fs,
                        &network,
                        scratch,
                        &mut staging,
                    ),
                    || network.borrow_mut().io_tick(),
                )
                .and_then(|r| r)
            };
            match operation {
                Ok(files::Step::Progress) => progress[slot] = port.borrow_mut().now_us(),
                Ok(files::Step::Waiting) => {}
                Ok(files::Step::Done) => {
                    requests += 1;
                    let _ = writeln!(
                        network.borrow_mut().console,
                        "WEB FILE DONE slot={} requests={}",
                        slot,
                        requests
                    );
                }
                Err(error) => {
                    let cleanup = esp32_p4::runtime::run(
                        files::cancel(&mut transfers[slot], &mut fs, &mut staging, slot),
                        || network.borrow_mut().io_tick(),
                    )
                    .and_then(|r| r);
                    {
                        let mut net = network.borrow_mut();
                        net.tcp.abort(slot);
                        net.tcp.release(slot);
                    }
                    requests += 1;
                    let _ = writeln!(
                        network.borrow_mut().console,
                        "WEB FILE ERROR slot={} {:?}",
                        slot,
                        error
                    );
                    if cleanup.is_err() || fs.is_poisoned() {
                        break 'service Err(web::Error::Storage);
                    }
                }
            }
        }
        turn = (turn + 1) % tcp::SLOTS;
        let (slot, request) = {
            let mut net = network.borrow_mut();
            if let Err(error) = net.tick() {
                break if net.stop || port.borrow_mut().now_us() >= net.deadline {
                    Ok(())
                } else {
                    Err(error)
                };
            }
            let Some(slot) = (0..tcp::SLOTS).find(|&i| {
                net.tcp.request_pending(i)
                    && (http::header_end(net.tcp.buffered(i)).is_some()
                        || net.tcp.buffered(i).len() == tcp::HEADER)
            }) else {
                continue;
            };
            let end = http::header_end(net.tcp.buffered(slot));
            net.tcp.hold(slot);
            let request = end
                .ok_or(431u16)
                .and_then(|n| http::parse(&net.tcp.buffered(slot)[..n]));
            if let Some(n) = end {
                net.tcp.consume(slot, n);
            }
            (slot, request)
        };
        if let Ok(ref r) = request {
            if r.route == http::Route::File
                && matches!(
                    r.method,
                    http::Method::Get | http::Method::Head | http::Method::Put | http::Method::Post
                )
            {
                let _ = writeln!(
                    network.borrow_mut().console,
                    "WEB FILE START slot={} {:?} {} bytes={}",
                    slot,
                    r.method,
                    r.path(),
                    r.length
                );
                let mut connection = Connection {
                    network: &network,
                    slot,
                    buffer: [0; tcp::MSS],
                    used: 0,
                };
                #[cfg(feature = "diagnostics")]
                let trace = {
                    let mut net = network.borrow_mut();
                    let losses = net.sample_losses();
                    net.max_tick_gap = 0;
                    // Separate statements drop the timer borrow before reading
                    // EMAC counters from the same RefCell (argument temporaries
                    // otherwise live until the entire function call ends).
                    let now = port.borrow_mut().now_us();
                    let emac = port.borrow().wired_stats().unwrap_or([0; 6]);
                    storage_lab::HttpTrace::begin(now, net.poll_us, losses, emac)
                };
                let started = port.borrow_mut().now_us();
                let result = esp32_p4::runtime::run(
                    files::start(r, &mut fs, &mut connection, &mut staging, slot, started),
                    || network.borrow_mut().io_tick(),
                )
                .and_then(|r| r);
                match result {
                    #[allow(unused_mut)]
                    Ok(mut transfer) => {
                        #[cfg(feature = "diagnostics")]
                        transfer.set_trace(trace, port.borrow_mut().now_us());
                        transfers[slot] = transfer;
                        progress[slot] = port.borrow_mut().now_us();
                    }
                    Err(error) => {
                        let mut net = network.borrow_mut();
                        net.tcp.abort(slot);
                        net.tcp.release(slot);
                        let _ = writeln!(net.console, "WEB FILE START ERROR {:?}", error);
                        if fs.is_poisoned() {
                            break 'service Err(web::Error::Storage);
                        }
                    }
                }
                continue;
            }
        }
        #[cfg(feature = "wifi")]
        let diagnostics = if request
            .as_ref()
            .is_ok_and(|r| r.route == http::Route::NetTest)
        {
            port.borrow().diagnostics()
        } else {
            None
        };
        let measuring = request
            .as_ref()
            .is_ok_and(|r| r.route == http::Route::NetTest);
        if measuring {
            port.borrow_mut().reset_stats();
            let mut n = network.borrow_mut();
            n.ticks = 0;
            n.poll_us = 0;
            n.read_empty = 0;
            n.write_full = 0;
        }
        let request_started = port.borrow_mut().now_us();
        let mut connection = Connection {
            network: &network,
            slot,
            buffer: [0; tcp::MSS],
            used: 0,
        };
        let result = match request {
            Ok(ref r) => {
                let _ = writeln!(
                    network.borrow_mut().console,
                    "WEB REQUEST {:?} {} bytes={}",
                    r.method,
                    r.path(),
                    r.length
                );
                if matches!(
                    r.route,
                    http::Route::NetTest
                        | http::Route::Clipboard
                        | http::Route::State
                        | http::Route::Brightness
                ) {
                    struct Controls<'a> {
                        panel: &'a mut esp32_p4::devices::jd9165::Display,
                        uptime: u64,
                        capacity: u64,
                    }
                    impl web::Control for Controls<'_> {
                        fn state(&mut self, out: &mut http::Text<512>) -> Result<(), web::Error> {
                            write!(out, "{{\"brightness\":{},\"uptime_us\":{},\"capacity_bytes\":{},\"clipboard_limit\":1024}}", self.panel.brightness(), self.uptime, self.capacity).map_err(|_| web::Error::Format)
                        }
                        fn brightness(&mut self, value: u8) -> Result<(), web::Error> {
                            self.panel.set_brightness(value);
                            Ok(())
                        }
                    }
                    let mut controls = Controls {
                        panel,
                        uptime: port.borrow_mut().now_us(),
                        capacity: fs.volume_capacity(),
                    };
                    let clipboard = unsafe { &mut *core::ptr::addr_of_mut!(CLIPBOARD) };
                    web::serve_control(r, &mut connection, scratch, clipboard, &mut controls)
                } else {
                    esp32_p4::runtime::run(
                        web::asyncfs::serve(r, &mut fs, &mut connection, &mut asset, scratch),
                        || network.borrow_mut().io_tick(),
                    )
                    .and_then(|r| r)
                }
            }
            Err(code) => web::error(&mut connection, code, false),
        };
        let result = result.and_then(|()| connection.finish());
        if result.is_err() {
            network.borrow_mut().tcp.abort(slot);
        }
        network.borrow_mut().tcp.release(slot);
        if measuring {
            let wired = port.borrow().wired_stats();
            if let Some(stats) = wired {
                let elapsed = port.borrow_mut().now_us() - request_started;
                let mut n = network.borrow_mut();
                let counters = (n.ticks, n.poll_us, n.read_empty, n.write_full);
                let _ = writeln!(
                    n.console,
                    "EMAC NET us={} {:?} poll=(calls,us,read_empty,write_full) {:?}",
                    elapsed, stats, counters
                );
            }
        }
        #[cfg(feature = "wifi")]
        if let Some(before) = diagnostics {
            let after = port.borrow().diagnostics().unwrap();
            let elapsed = port.borrow_mut().now_us() - request_started;
            let _ = writeln!(
                network.borrow_mut().console,
                "C6 NET us={} {:?}",
                elapsed,
                after.since(before)
            );
        }
        requests += 1;
        let _ = writeln!(
            network.borrow_mut().console,
            "WEB RESPONSE {:?} requests={}",
            result,
            requests
        );
    };
    let _ = writeln!(
        network.borrow_mut().console,
        "WEB STAGING allocated_slots={} peak_active={} chunk={} sd_bounce={}",
        staging.stats().0,
        staging.stats().1,
        web::upload::CHUNK,
        hal::sdmmc::MAX_TRANSFER
    );
    let cleanup = esp32_p4::runtime::run(fs.shutdown(), || network.borrow_mut().io_tick())
        .and_then(|r| r.map_err(|_| web::Error::Storage));
    for (slot, transfer) in transfers.iter().enumerate() {
        if !transfer.idle() {
            let mut n = network.borrow_mut();
            n.tcp.abort(slot);
            n.tcp.release(slot);
        }
    }
    let mut net = network.borrow_mut();
    let net = &mut *net;
    let _ = writeln!(
        net.console,
        "WEB SUMMARY requests={} rx_errors={} tcp={:?} irq={:?} port={:08x?}",
        requests,
        net.rx_errors,
        net.tcp.stats,
        hal::interrupt::stats(),
        port.borrow().status()
    );
    outcome.and(cleanup)
}
