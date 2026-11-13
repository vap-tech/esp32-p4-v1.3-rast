//! Board integration for the bounded TCP/HTTP file server. Single cooperative
//! task owns SD+EMAC. RefCell serializes their access to the sole SYSTIMER handle.
use core::{
    cell::{Cell, RefCell},
    fmt::Write,
};
use esp32_p4::{
    hal, memory,
    net::{self, http, tcp},
    storage::{
        asynchronous::{self as disk},
        volume::Volume,
    },
    web::{self, Asset, Transport},
};
/// One owner serializes controller I/O and lends its sole timer to SD.
pub(crate) trait Port {
    fn diagnostics(&self) -> Option<crate::wifi::Stats> {
        None
    }

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
struct Card<'a, P: Port> {
    card: hal::sdmmc::Sdmmc,
    port: &'a RefCell<P>,
    busy: &'a Cell<bool>,
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
        hal::sdmmc::MAX_TRANSFER
    }
    async fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), Self::Error> {
        esp32_p4::runtime::yield_now().await;
        let lba = u32::try_from(lba).map_err(|_| Self::Error::AddressOverflow)?;
        let mut request = self
            .card
            .request_read(lba, out.len())
            .map_err(Self::Error::Card)?;
        assert!(!self.busy.replace(true));
        let _busy = Busy(self.busy);
        core::future::poll_fn(|cx| {
            self.port
                .borrow_mut()
                .with_timer(|timer| self.card.poll_request(&mut request, cx, timer, out, None))
        })
        .await
        .map_err(Self::Error::Card)
    }
    async fn write(&mut self, lba: u64, bytes: &[u8]) -> Result<(), Self::Error> {
        esp32_p4::runtime::yield_now().await;
        let lba = u32::try_from(lba).map_err(|_| Self::Error::AddressOverflow)?;
        let mut request = self
            .card
            .request_write(lba, bytes.len())
            .map_err(Self::Error::Card)?;
        assert!(!self.busy.replace(true));
        let _busy = Busy(self.busy);
        core::future::poll_fn(|cx| {
            self.port.borrow_mut().with_timer(|timer| {
                self.card
                    .poll_request(&mut request, cx, timer, &mut [], Some(bytes))
            })
        })
        .await
        .map_err(Self::Error::Card)
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
/// smoltcp tokens borrow frame workspaces only until the synchronous DMA call ends.
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
    busy: &'a Cell<bool>,
}
impl<P: Port> Network<'_, '_, P> {
    fn tick(&mut self) -> Result<(), web::Error> {
        if self.console.read_byte() == Some(27) {
            self.stop = true;
        }
        let now = self.port.borrow_mut().now_us();
        if self.stop || now >= self.deadline {
            return Err(web::Error::Transport);
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
            at += self
                .network
                .borrow_mut()
                .tcp
                .write(self.slot, &self.buffer[at..self.used]);
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
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    panel: &mut esp32_p4::devices::jd9165::Display,
) -> Result<(), web::Error> {
    let cache = hal::memory::cache_snapshot();
    if cache.l1_bypass & 16 == 0 || cache.l2_size != 0 {
        return Err(web::Error::CacheConfiguration);
    }
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
    serve(&port, card, arena, seconds, console, panel)
}
pub fn run_wifi(
    seconds: u32,
    arena: &memory::Arena<'_, memory::Internal>,
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
            match serve(&port, card, arena, seconds, console, panel) {
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
) -> Result<(), web::Error> {
    let mut asset = FlashPage::open()?;
    let xid = port.borrow_mut().now_us() as u32;
    let busy = Cell::new(false);
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
        busy: &busy,
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
    let partition = esp32_p4::runtime::run(
        disk::Partition::mbr(
            Card {
                card,
                port,
                busy: &busy,
            },
            0,
        ),
        || network.borrow_mut().tick(),
    )?
    .map_err(|_| web::Error::Storage)?;
    let mut stream = RefCell::new(disk::Stream::new(partition));
    let mut fs =
        esp32_p4::runtime::run(Volume::mount(&mut stream), || network.borrow_mut().tick())?
            .map_err(|_| web::Error::Storage)?;
    let _ = writeln!(
        network.borrow_mut().console,
        "WEB READY http://{}.{}.{}.{}/ seconds={} FS=async-fat ESC=stop",
        a[0],
        a[1],
        a[2],
        a[3],
        seconds
    );
    let mut scratch_allocation = arena
        .allocate(core::alloc::Layout::from_size_align(8192, 64).unwrap())
        .map_err(|_| web::Error::Storage)?;
    let scratch = scratch_allocation.as_mut_slice();
    let mut requests = 0;
    let outcome = loop {
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
        let diagnostics = if request
            .as_ref()
            .is_ok_and(|r| r.route == http::Route::NetTest)
        {
            port.borrow().diagnostics()
        } else {
            None
        };
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
                        capacity: fs.capacity(),
                    };
                    let clipboard = unsafe { &mut *core::ptr::addr_of_mut!(CLIPBOARD) };
                    web::serve_control(r, &mut connection, scratch, clipboard, &mut controls)
                } else {
                    esp32_p4::runtime::run(
                        web::asyncfs::serve(r, &mut fs, &mut connection, &mut asset, scratch),
                        || network.borrow_mut().tick(),
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
    outcome
}
