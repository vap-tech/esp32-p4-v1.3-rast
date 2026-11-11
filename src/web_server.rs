//! Board integration for the bounded TCP/HTTP file server. Single cooperative
//! task owns SD+EMAC. RefCell serializes their access to the sole SYSTIMER handle.
use core::{cell::RefCell, fmt::Write};
use esp32_p4::{
    hal, memory,
    net::{self, http, tcp},
    storage::{fat32::Fat32, BlockDevice, Partition},
    web::{self, Asset, Transport},
};
type Mac<'a, 't> = RefCell<hal::ethernet::Emac<'a, 't>>;
static mut TCP: tcp::Stack = tcp::Stack::new();
static mut SCRATCH: [u8; 8192] = [0; 8192];
struct Card<'a, 'm, 't> {
    card: hal::sdmmc::Sdmmc,
    mac: &'a Mac<'m, 't>,
}
impl BlockDevice for Card<'_, '_, '_> {
    type Error = esp32_p4::storage::sd::Error;
    fn sectors(&self) -> u64 {
        self.card.info.sectors
    }
    fn max_transfer_bytes(&self) -> usize {
        hal::sdmmc::MAX_TRANSFER
    }
    fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), Self::Error> {
        let lba = u32::try_from(lba).map_err(|_| Self::Error::AddressOverflow)?;
        self.mac
            .borrow_mut()
            .with_timer(|t| self.card.read_sectors(lba, out, t))
            .map_err(Self::Error::Card)
    }
    fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), Self::Error> {
        let lba = u32::try_from(lba).map_err(|_| Self::Error::AddressOverflow)?;
        self.mac
            .borrow_mut()
            .with_timer(|t| self.card.write_sectors(lba, data, t))
            .map_err(Self::Error::Card)
    }
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
struct Network<'a, 'm, 't, 'c> {
    mac: &'a Mac<'m, 't>,
    tcp: &'a mut tcp::Stack,
    console: &'c mut hal::usb_serial::Console,
    rx: [u8; net::FRAME],
    tx: [u8; net::FRAME],
    ip: [u8; 4],
    deadline: u64,
    stop: bool,
    rx_errors: u32,
}
impl Network<'_, '_, '_, '_> {
    fn tick(&mut self) -> Result<(), web::Error> {
        if self.console.read_byte() == Some(27) {
            self.stop = true;
        }
        let now = self.mac.borrow_mut().now_us();
        if self.stop || now >= self.deadline {
            return Err(web::Error::Transport);
        }
        let n = match self.mac.borrow_mut().receive(&mut self.rx) {
            Ok(n) => n,
            Err(hal::ethernet::Error::Frame(_)) => {
                self.rx_errors += 1;
                None
            }
            Err(_) => return Err(web::Error::Transport),
        };
        if let Some(n) = n {
            if let Some(len) = net::reply(&self.rx[..n], self.ip, &mut self.tx) {
                self.mac
                    .borrow_mut()
                    .transmit(&self.tx[..len])
                    .map_err(|_| web::Error::Transport)?;
            }
            if let Some(len) = self.tcp.ingest(&self.rx[..n], self.ip, now, &mut self.tx) {
                self.mac
                    .borrow_mut()
                    .transmit(&self.tx[..len])
                    .map_err(|_| web::Error::Transport)?;
            }
        }
        if let Some(n) = self.tcp.poll(self.ip, now, &mut self.tx) {
            self.mac
                .borrow_mut()
                .transmit(&self.tx[..n])
                .map_err(|_| web::Error::Transport)?;
        }
        Ok(())
    }
}
struct Connection<'a, 'n, 'm, 't, 'c> {
    network: &'a mut Network<'n, 'm, 't, 'c>,
    slot: usize,
    buffer: [u8; tcp::MSS],
    used: usize,
}
impl Transport for Connection<'_, '_, '_, '_, '_> {
    fn read(&mut self, out: &mut [u8]) -> Result<usize, web::Error> {
        let started = self.network.mac.borrow_mut().now_us();
        loop {
            if self.network.mac.borrow_mut().now_us() - started > 30_000_000 {
                return Err(web::Error::Transport);
            }
            let n = self.network.tcp.read(self.slot, out);
            if n != 0 {
                return Ok(n);
            }
            if self.network.tcp.eof(self.slot) {
                return Ok(0);
            }
            self.network.tick()?;
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
        let started = self.network.mac.borrow_mut().now_us();
        while at < self.used || !self.network.tcp.flushed(self.slot) {
            if self.network.mac.borrow_mut().now_us() - started > 30_000_000 {
                return Err(web::Error::Transport);
            }
            if !self.network.tcp.alive(self.slot) {
                return Err(web::Error::Transport);
            }
            if at < self.used {
                at += self
                    .network
                    .tcp
                    .write(self.slot, &self.buffer[at..self.used]);
            }
            self.network.tick()?;
        }
        self.used = 0;
        Ok(())
    }
}
#[inline(never)]
pub fn run(
    seconds: u32,
    arena: &memory::Arena<'_, memory::Internal>,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
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
    let mut asset = FlashPage::open()?;
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(|_| web::Error::Storage)?;
    card.configure_bus(timer).map_err(|_| web::Error::Storage)?;
    let xid = timer.now_us() as u32;
    let mac = RefCell::new(
        unsafe { hal::ethernet::Emac::start(arena, timer, net::MAC) }
            .map_err(|_| web::Error::Transport)?,
    );
    let device = Card { card, mac: &mac };
    let partition = Partition::mbr(device, 0).map_err(|_| web::Error::Storage)?;
    let mut fs = Fat32::mount(partition).map_err(|_| web::Error::Storage)?;
    // Sole entry point owns this static stack for the entire synchronous run.
    let tcp = unsafe { &mut *core::ptr::addr_of_mut!(TCP) };
    tcp.clear();
    let start = mac.borrow_mut().now_us();
    let mut network = Network {
        mac: &mac,
        tcp,
        console,
        rx: [0; net::FRAME],
        tx: [0; net::FRAME],
        ip: [0; 4],
        deadline: start + u64::from(seconds) * 1_000_000,
        stop: false,
        rx_errors: 0,
    };
    let mut offer = None;
    let mut next = 0;
    let mut lease = None;
    while mac.borrow_mut().now_us() < network.deadline {
        let now = mac.borrow_mut().now_us();
        if now >= next {
            let n = net::dhcp(&mut network.tx, xid, offer);
            mac.borrow_mut()
                .transmit(&network.tx[..n])
                .map_err(|_| web::Error::Transport)?;
            next = now + 3_000_000;
        }
        if network.console.read_byte() == Some(27) {
            return Ok(());
        }
        let n = mac
            .borrow_mut()
            .receive(&mut network.rx)
            .map_err(|_| web::Error::Transport)?;
        if let Some(n) = n {
            if let Some((kind, got)) = net::dhcp_reply(&network.rx[..n], xid) {
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
    network.deadline = network
        .deadline
        .min(mac.borrow_mut().now_us() + u64::from(lease.seconds) * 1_000_000);
    let a = network.ip;
    let _ = writeln!(
        network.console,
        "WEB READY http://{}.{}.{}.{}/ seconds={} ESC=stop",
        a[0], a[1], a[2], a[3], seconds
    );
    let scratch = unsafe { &mut *core::ptr::addr_of_mut!(SCRATCH) };
    let mut requests = 0;
    let outcome = loop {
        if let Err(error) = network.tick() {
            break if network.stop || mac.borrow_mut().now_us() >= network.deadline {
                Ok(())
            } else {
                Err(error)
            };
        }
        let Some(slot) = (0..tcp::SLOTS).find(|&i| {
            network.tcp.request_pending(i)
                && (http::header_end(network.tcp.buffered(i)).is_some()
                    || network.tcp.buffered(i).len() == tcp::RX)
        }) else {
            continue;
        };
        // Do not let an idle preconnection prevent service of another socket.
        let end = http::header_end(network.tcp.buffered(slot));
        if end.is_none() && network.tcp.buffered(slot).len() < tcp::RX {
            continue;
        }
        network.tcp.hold(slot);
        let request = end
            .ok_or(431u16)
            .and_then(|n| http::parse(&network.tcp.buffered(slot)[..n]));
        if let Some(n) = end {
            network.tcp.consume(slot, n);
        }
        let mut connection = Connection {
            network: &mut network,
            slot,
            buffer: [0; tcp::MSS],
            used: 0,
        };
        let result = match request {
            Ok(ref r) => {
                let _ = writeln!(
                    connection.network.console,
                    "WEB REQUEST {:?} {} bytes={}",
                    r.method,
                    r.path(),
                    r.length
                );
                web::serve(r, &mut fs, &mut connection, &mut asset, scratch)
            }
            Err(code) => web::error(&mut connection, code, false),
        };
        if result.is_err() {
            connection.network.tcp.abort(slot);
        }
        connection.network.tcp.release(slot);
        requests += 1;
        let _ = writeln!(
            connection.network.console,
            "WEB RESPONSE {:?} requests={}",
            result, requests
        );
    };
    let _ = writeln!(
        network.console,
        "WEB SUMMARY requests={} rx_errors={} tcp={:?} mac={:08x?}",
        requests,
        network.rx_errors,
        network.tcp.stats,
        mac.borrow().status()
    );
    outcome
}
