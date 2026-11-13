//! ESP-Hosted SDIO bring-up and bounded RPC v2 diagnostics. No C6 firmware updates.
use core::fmt::Write;
use esp32_p4::{
    hal::{self, sdmmc::Sdio},
    net::{
        self,
        hosted::{self, Encoder, Value},
    },
};
#[link_section = ".dma.fixed"]
static mut RX: [u8; 16384] = [0; 16384];
#[link_section = ".dma.fixed"]
static mut TX: [u8; 2048] = [0; 2048];
/// Cumulative bus costs. Snapshots are logged only after a RAM HTTP test.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stats {
    rx_polls: u64,
    rx_empty: u64,
    reg_reads: u64,
    reg_us: u64,
    rx_transfers: u64,
    rx_bytes: u64,
    rx_us: u64,
    tx_transfers: u64,
    tx_bytes: u64,
    tx_us: u64,
    credit_stalls: u64,
    credit_wait_us: u64,
}
impl Stats {
    pub fn since(self, old: Self) -> Self {
        Self {
            rx_polls: self.rx_polls - old.rx_polls,
            rx_empty: self.rx_empty - old.rx_empty,
            reg_reads: self.reg_reads - old.reg_reads,
            reg_us: self.reg_us - old.reg_us,
            rx_transfers: self.rx_transfers - old.rx_transfers,
            rx_bytes: self.rx_bytes - old.rx_bytes,
            rx_us: self.rx_us - old.rx_us,
            tx_transfers: self.tx_transfers - old.tx_transfers,
            tx_bytes: self.tx_bytes - old.tx_bytes,
            tx_us: self.tx_us - old.tx_us,
            credit_stalls: self.credit_stalls - old.credit_stalls,
            credit_wait_us: self.credit_wait_us - old.credit_wait_us,
        }
    }
}
pub(crate) struct Link {
    sdio: Sdio,
    stats: Stats,
    chip: u8,
    raw_tp: u8,
    received: u32,
    sent: u32,
    sequence: u16,
    uid: u32,
    checksum: bool,
    connected: bool,
    disconnected: bool,
}
impl Link {
    fn host_caps(&mut self, raw: u8, timer: &mut hal::time::Timer) -> Result<(), &'static str> {
        self.send(
            5,
            &[
                0x22, 18, 0x44, 1, 0, 0x45, 1, self.chip, 0x46, 1, raw, 0x47, 1, 0, 0x48, 1, 0,
                0x1a, 1, 2,
            ],
            timer,
        )
    }
    fn register(
        &mut self,
        address: u32,
        timer: &mut hal::time::Timer,
    ) -> Result<u32, &'static str> {
        let start = timer.now_us();
        let value = self.sdio.read_register(address, timer);
        self.stats.reg_reads += 1;
        self.stats.reg_us += timer.now_us() - start;
        value.map_err(|_| "SDIO register")
    }
    fn receive(
        &mut self,
        timer: &mut hal::time::Timer,
        out: &mut [u8],
    ) -> Result<usize, &'static str> {
        self.stats.rx_polls += 1;
        let total = self.register(0x60, timer)? & 0xfffff;
        let len = total.wrapping_sub(self.received) & 0xfffff;
        if len == 0 {
            self.stats.rx_empty += 1;
            return Ok(0);
        }
        let rounded = (len as usize).next_multiple_of(512);
        if rounded > out.len() {
            return Err("RX aggregate exceeds bounded workspace");
        }
        let start = timer.now_us();
        let result = self.sdio.receive(0x1f800 - len, &mut out[..rounded], timer);
        self.stats.rx_us += timer.now_us() - start;
        result.map_err(|_| "RX CMD53")?;
        self.stats.rx_transfers += 1;
        self.stats.rx_bytes += u64::from(len);
        self.received = total;
        Ok(len as usize)
    }
    fn send(
        &mut self,
        interface: u8,
        payload: &[u8],
        timer: &mut hal::time::Timer,
    ) -> Result<(), &'static str> {
        let tx = unsafe { &mut *core::ptr::addr_of_mut!(TX) };
        let len = hosted::encode_frame(tx, interface, self.sequence, payload, self.checksum)?;
        tx[len..len.next_multiple_of(512)].fill(0);
        let start = timer.now_us();
        let mut waiting = false;
        loop {
            let credits = (self.register(0x44, timer)? >> 16) & 0xfff;
            if credits.wrapping_sub(self.sent) & 0xfff != 0 {
                if waiting {
                    self.stats.credit_wait_us += timer.now_us() - start;
                }
                break;
            }
            waiting = true;
            self.stats.credit_stalls += 1;
            if timer.now_us().wrapping_sub(start) > 1_000_000 {
                return Err("no TX credit");
            }
            timer.delay_us(1000);
        }
        let start = timer.now_us();
        let result = self.sdio.transmit(
            0x1f800 - len as u32,
            &tx[..len.next_multiple_of(512)],
            timer,
        );
        self.stats.tx_us += timer.now_us() - start;
        result.map_err(|_| "TX CMD53")?;
        self.stats.tx_transfers += 1;
        self.stats.tx_bytes += len as u64;
        self.sent = (self.sent + 1) & 0xfff;
        self.sequence = self.sequence.wrapping_add(1);
        Ok(())
    }
    fn event(&mut self, kind: u64, id: u64, console: &mut hal::usb_serial::Console) {
        if kind == 3 && id == 775 {
            self.connected = true;
            self.disconnected = false;
            let _ = writeln!(console, "C6 STA CONNECTED");
        }
        if kind == 3 && id == 776 {
            self.connected = false;
            self.disconnected = true;
            let _ = writeln!(console, "C6 STA DISCONNECTED");
        }
    }
    fn event_packet(
        &mut self,
        payload: &[u8],
        console: &mut hal::usb_serial::Console,
    ) -> Result<(), &'static str> {
        let mut p = hosted::serial(payload)?;
        let (mut kind, mut id) = (0, 0);
        while let Some((field, value)) = hosted::field(&mut p)? {
            match (field, value) {
                (1, Value::Scalar(v)) => kind = v,
                (2, Value::Scalar(v)) => id = v,
                _ => {}
            }
        }
        self.event(kind, id, console);
        Ok(())
    }
    fn rpc(
        &mut self,
        id: u32,
        body: &[u8],
        timer: &mut hal::time::Timer,
        console: &mut hal::usb_serial::Console,
        out: &mut [u8],
    ) -> Result<usize, &'static str> {
        self.uid = self.uid.wrapping_add(1);
        let mut proto = [0; 256];
        let mut e = Encoder::new(&mut proto);
        e.scalar(1, 1)?;
        e.scalar(2, u64::from(id))?;
        e.scalar(3, u64::from(self.uid))?;
        e.bytes(id, body)?;
        let plen = e.len;
        let mut serial = [0; 288];
        serial[..9].copy_from_slice(b"\x01\x06\x00RPCRsp");
        serial[9] = 2;
        serial[10..12].copy_from_slice(&(plen as u16).to_le_bytes());
        serial[12..12 + plen].copy_from_slice(&proto[..plen]);
        self.send(3, &serial[..12 + plen], timer)?;
        let _ = writeln!(console, "C6 RPC TX id={id} uid={}", self.uid);
        let start = timer.now_us();
        loop {
            if console.read_byte() == Some(27) {
                return Err("cancelled");
            }
            if timer.now_us().wrapping_sub(start) > 15_000_000 {
                return Err("RPC timeout");
            }
            let rx = unsafe { &mut *core::ptr::addr_of_mut!(RX) };
            let len = self.receive(timer, rx)?;
            let mut at = 0;
            let mut matched = None;
            while at < len {
                // Hardware aggregation may pad the last frame to a four-byte boundary.
                if rx[at..len].iter().all(|b| *b == 0) {
                    break;
                }
                let (interface, payload, used) = hosted::frame(&rx[at..len])?;
                at += used;
                if interface != 3 {
                    continue;
                }
                let mut proto = hosted::serial(payload)?;
                let (mut kind, mut got_id, mut uid, mut body) = (0, 0, 0, None);
                while let Some((field, value)) = hosted::field(&mut proto)? {
                    match (field, value) {
                        (1, Value::Scalar(v)) => kind = v,
                        (2, Value::Scalar(v)) => got_id = v,
                        (3, Value::Scalar(v)) => uid = v,
                        (f, Value::Bytes(b)) if f == id + 256 => body = Some(b),
                        _ => {}
                    }
                }
                self.event(kind, got_id, console);
                let _ = writeln!(console, "C6 RPC RX type={kind} id={got_id} uid={uid}");
                if kind == 2 && got_id == u64::from(id + 256) && uid == u64::from(self.uid) {
                    let body = body.ok_or("RPC payload missing")?;
                    if body.len() > out.len() {
                        return Err("RPC reply too large");
                    }
                    out[..body.len()].copy_from_slice(body);
                    matched = Some(body.len());
                }
            }
            if let Some(n) = matched {
                return Ok(n);
            }
            timer.delay_us(1000);
        }
    }
}
fn result(bytes: &[u8]) -> Result<(), &'static str> {
    let mut b = bytes;
    while let Some((n, v)) = hosted::field(&mut b)? {
        if let (1, Value::Scalar(code)) = (n, v) {
            if code != 0 {
                return Err("C6 RPC returned error");
            }
        }
    }
    Ok(())
}
fn open(
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    reset_active_high: bool,
) -> Result<Link, &'static str> {
    if hal::memory::cache_snapshot().l1_bypass & 16 == 0 {
        return Err("D-cache must be off");
    }
    let _=writeln!(console,"C6 START slot=1 clk=18 cmd=19 data=14..17 reset=54 active_high={reset_active_high} 1bit/400kHz");
    let mut sdio = unsafe { Sdio::new(timer, reset_active_high) }.map_err(|e| {
        let _ = writeln!(
            console,
            "C6 SDIO INIT {e:?} DEBUG={:08x?}",
            hal::sdmmc::sdio_debug()
        );
        "SDIO init"
    })?;
    let _ = writeln!(
        console,
        "C6 SDIO ocr={:08x} cccr={:02x?}",
        sdio.ocr, sdio.cccr
    );
    sdio.enable(timer).map_err(|_| "function enable")?;
    sdio.four_bit_25mhz(timer).map_err(|_| "4bit/25MHz")?;
    let _ = writeln!(console, "C6 BUS 4bit/25MHz");
    sdio.write_byte(1, 0x8c, 1, timer)
        .map_err(|_| "open data path")?;
    let mut link = Link {
        sdio,
        stats: Stats::default(),
        chip: 0x0d,
        raw_tp: 0,
        received: 0,
        sent: 0,
        sequence: 0,
        uid: 0,
        checksum: false,
        connected: false,
        disconnected: false,
    };
    let start = timer.now_us();
    let info = loop {
        if console.read_byte() == Some(27) {
            return Err("cancelled");
        }
        let rx = unsafe { &mut *core::ptr::addr_of_mut!(RX) };
        let n = link.receive(timer, rx)?;
        if n != 0 {
            let (interface, payload, _) = hosted::frame(&rx[..n])?;
            if interface != 5 {
                return Err("expected boot private event");
            }
            break hosted::boot(payload)?;
        }
        if timer.now_us().wrapping_sub(start) > 5_000_000 {
            return Err("startup timeout");
        }
        timer.delay_us(1000);
    };
    let _=writeln!(console,"C6 BOOT chip={:02x} version={}.{}.{} rpc={} capabilities={:02x} ext={:08x} mode={} buffers={:?}",info.chip,info.version>>16,(info.version>>8)&255,info.version&255,info.rpc,info.capabilities,info.extended,info.sdio_mode,info.buffer_config);
    link.checksum = info.capabilities & 128 != 0;
    link.chip = info.chip;
    link.raw_tp = info.raw_tp;
    let _ = writeln!(
        console,
        "C6 RAW capability={:02x} registers={}",
        info.raw_tp,
        if cfg!(feature = "sdio-reg-cmd52") {
            "CMD52x4"
        } else {
            "CMD53x1"
        }
    );
    link.host_caps(0, timer)?;
    Ok(link)
}
/// Pure SDIO transport test. No Wi-Fi association, IP/TCP, SD card or C6 Flash writes.
#[cfg(feature = "diagnostics")]
pub fn raw(
    args: &str,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    let mut args = args.split_ascii_whitespace();
    let mode = match args.next() {
        Some("rx") => 2,
        Some("tx") => 4,
        _ => return Err("raw rx|tx seconds"),
    };
    let seconds = args
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|s| (1..=30).contains(s))
        .ok_or("seconds 1..30")?;
    if args.next().is_some() {
        return Err("raw rx|tx seconds");
    }
    let mut link = open(timer, console, false)?;
    if link.raw_tp & 1 == 0 {
        return Err("C6 raw test unsupported");
    }
    let mut payload = [0x5a; 1460];
    let mut tx_frames = 0u32;
    let mut rx_frames = 0u32;
    let mut rx_bytes = 0u64;
    let mut measured = None;
    let wait_start = timer.now_us();
    let mut before = link.stats;
    link.host_caps(mode, timer)?;
    let _ = writeln!(
        console,
        "C6 RAW START mode={} seconds={} warmup<=8s",
        if mode == 2 { "rx" } else { "tx" },
        seconds
    );
    let outcome = (|| {
        loop {
            if console.read_byte() == Some(27) {
                break;
            }
            let now = timer.now_us();
            if measured.is_some_and(|start| now - start >= u64::from(seconds) * 1_000_000) {
                break;
            }
            if measured.is_none() && now - wait_start > 8_000_000 {
                return Err("no raw RX from C6");
            }
            if mode == 4 {
                if measured.is_none() {
                    measured = Some(now);
                    before = link.stats;
                }
                payload[..4].copy_from_slice(&tx_frames.wrapping_add(1).to_le_bytes());
                link.send(6, &payload, timer)?;
                tx_frames += 1;
            }
            let rx = unsafe { &mut *core::ptr::addr_of_mut!(RX) };
            let n = link.receive(timer, rx)?;
            let mut cursor = 0;
            while cursor < n {
                if rx[cursor..n].iter().all(|b| *b == 0) {
                    break;
                }
                let (interface, payload) = hosted::stream_frame(&rx[..n], &mut cursor)?;
                if interface == 6 {
                    if measured.is_none() {
                        measured = Some(timer.now_us());
                        before = link.stats;
                    }
                    rx_frames += 1;
                    rx_bytes += payload.len() as u64;
                }
            }
        }
        Ok(())
    })();
    let elapsed = measured.map_or(0, |start| timer.now_us() - start);
    let stop = link.host_caps(0, timer);
    let _ = writeln!(
        console,
        "C6 RAW SUMMARY measured_us={} tx_frames={} tx_payload={} rx_frames={} rx_payload={} {:?}",
        elapsed,
        tx_frames,
        u64::from(tx_frames) * 1460,
        rx_frames,
        rx_bytes,
        link.stats.since(before)
    );
    // Fully reset the temporary test producer and host FIFOs, even on error/ESC.
    let reset = unsafe { Sdio::new(timer, false) }.map_err(|_| "raw cleanup reset");
    outcome.and(stop).and(reset.map(|_| ()))
}
pub fn probe(
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    reset_active_high: bool,
    scan: bool,
) -> Result<(), &'static str> {
    let mut link = open(timer, console, reset_active_high)?;
    let mut response = [0; 1024];
    let n = link.rpc(267, &[], timer, console, &mut response)?;
    result(&response[..n])?;
    let mut description = &response[..n];
    while let Some((field, value)) = hosted::field(&mut description)? {
        if let (2, Value::Bytes(mut desc)) = (field, value) {
            while let Some((field, value)) = hosted::field(&mut desc)? {
                if let (field @ (4 | 5 | 8), Value::Bytes(text)) = (field, value) {
                    let text = text.strip_suffix(&[0]).unwrap_or(text);
                    let _ = writeln!(
                        console,
                        "C6 APP field={} text={:?}",
                        field,
                        core::str::from_utf8(text).unwrap_or("<non-UTF8>")
                    );
                }
            }
        }
    }
    if !scan {
        return Ok(());
    }
    wifi_init(&mut link, timer, console)?;
    let scan_result = (|| {
        let n = link.rpc(286, &[16, 1], timer, console, &mut response)?;
        result(&response[..n])?; // blocking scan, default config
        let n = link.rpc(289, &[8, 8], timer, console, &mut response)?;
        result(&response[..n])?;
        let mut records = &response[..n];
        let mut count = 0;
        while let Some((field, value)) = hosted::field(&mut records)? {
            if let (3, Value::Bytes(mut ap)) = (field, value) {
                let (mut ssid, mut channel, mut rssi, mut auth) = (&b""[..], 0, 0, 0);
                while let Some((f, v)) = hosted::field(&mut ap)? {
                    match (f, v) {
                        (2, Value::Bytes(b)) => ssid = b,
                        (3, Value::Scalar(v)) => channel = v,
                        (5, Value::Scalar(v)) => rssi = v as i32,
                        (6, Value::Scalar(v)) => auth = v,
                        _ => {}
                    }
                }
                let name = &ssid[..ssid.iter().position(|b| *b == 0).unwrap_or(ssid.len())];
                let _ = writeln!(
                    console,
                    "C6 AP ssid={:?} channel={channel} rssi={rssi} auth={auth}",
                    core::str::from_utf8(name).unwrap_or("<non-UTF8>")
                );
                count += 1;
            }
        }
        let _ = writeln!(console, "C6 SCAN records={count}");
        Ok(())
    })();
    // Stop/deinit even after scan failure, without overwriting the original error.
    let stop = link
        .rpc(281, &[], timer, console, &mut response)
        .and_then(|n| result(&response[..n]));
    let deinit = link
        .rpc(279, &[], timer, console, &mut response)
        .and_then(|n| result(&response[..n]));
    scan_result.and(stop).and(deinit)
}

fn wifi_init(
    link: &mut Link,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    let mut response = [0; 1024];
    // Explicit modest radio buffers. nvs_enable=0: do not persist Wi-Fi config.
    let mut cfg = [0; 128];
    let mut e = Encoder::new(&mut cfg);
    for (f, v) in [
        (1, 8),
        (2, 16),
        (3, 1),
        (4, 0),
        (5, 16),
        (6, 8),
        (7, 0),
        (8, 1),
        (9, 1),
        (10, 0),
        (11, 0),
        (12, 0),
        (13, 6),
        (14, 0),
        (15, 752),
        (16, 16),
        (17, 0),
        (18, 0),
        (19, 7),
        (20, 0x1f2f3f4f),
    ] {
        e.scalar(f, v)?;
    }
    let clen = e.len;
    let mut init = [0; 144];
    let mut e = Encoder::new(&mut init);
    e.bytes(1, &cfg[..clen])?;
    let ilen = e.len;
    let n = link.rpc(278, &init[..ilen], timer, console, &mut response)?;
    let _ = writeln!(console, "C6 WIFI INIT {:02x?}", &response[..n]);
    result(&response[..n])?;
    let n = link.rpc(260, &[8, 1], timer, console, &mut response)?;
    result(&response[..n])?; // STA
    let n = link.rpc(280, &[], timer, console, &mut response)?;
    result(&response[..n])?;
    Ok(())
}

fn decode_hex(text: &str, out: &mut [u8]) -> Result<usize, &'static str> {
    if !text.len().is_multiple_of(2) || text.len() / 2 > out.len() {
        return Err("credential length");
    }
    fn digit(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    for (i, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        out[i] = digit(pair[0]).ok_or("credential encoding")? * 16
            + digit(pair[1]).ok_or("credential encoding")?;
    }
    Ok(text.len() / 2)
}
fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        unsafe { core::ptr::write_volatile(byte, 0) }
    }
}
pub fn connect(
    args: &str,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    let mut args = args.split_ascii_whitespace();
    let mut ssid = [0; 32];
    let mut password = [0; 64];
    let slen = decode_hex(args.next().ok_or("SSID missing")?, &mut ssid)?;
    let plen = decode_hex(args.next().ok_or("password missing")?, &mut password)?;
    let seconds = args
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|n| (1..=3600).contains(n))
        .ok_or("seconds 1..3600")?;
    if args.next().is_some()
        || slen == 0
        || ssid[..slen].contains(&0)
        || password[..plen].contains(&0)
        || !(8..=64).contains(&plen)
    {
        wipe(&mut password);
        return Err("credential format");
    }
    let result = station(&ssid[..slen], &password[..plen], seconds, timer, console);
    wipe(&mut password);
    result
}
pub fn connect_default(
    seconds: u32,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    if !(1..=3600).contains(&seconds) {
        return Err("seconds 1..3600");
    }
    station(
        crate::wifi_profile::SSID,
        crate::wifi_profile::PASSWORD,
        seconds,
        timer,
        console,
    )
}
fn station(
    ssid: &[u8],
    password: &[u8],
    seconds: u32,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    station_with(
        ssid,
        password,
        timer,
        console,
        |link, mac, timer, console| network(link, mac, seconds, timer, console),
    )
}
pub(crate) fn with_default_link(
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    service: impl FnOnce(
        &mut Link,
        [u8; 6],
        &mut hal::time::Timer,
        &mut hal::usb_serial::Console,
    ) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    station_with(
        crate::wifi_profile::SSID,
        crate::wifi_profile::PASSWORD,
        timer,
        console,
        service,
    )
}
fn station_with(
    ssid: &[u8],
    password: &[u8],
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    service: impl FnOnce(
        &mut Link,
        [u8; 6],
        &mut hal::time::Timer,
        &mut hal::usb_serial::Console,
    ) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    let run = (|| {
        let mut link = open(timer, console, false)?;
        let mut response = [0; 1024];
        let outcome = (|| {
            wifi_init(&mut link, timer, console)?;
            let n = link.rpc(270, &[8, 0], timer, console, &mut response)?;
            result(&response[..n])?;
            let _ = writeln!(console, "C6 POWER SAVE off (bring-up mode)");
            let n = link.rpc(257, &[], timer, console, &mut response)?;
            let mut bytes = &response[..n];
            let mut mac = [0; 6];
            let mut have_mac = false;
            while let Some((f, v)) = hosted::field(&mut bytes)? {
                match (f, v) {
                    (1, Value::Bytes(v)) if v.len() == 6 => {
                        mac.copy_from_slice(v);
                        have_mac = true;
                    }
                    (2, Value::Scalar(v)) if v != 0 => return Err("get MAC failed"),
                    _ => {}
                }
            }
            if !have_mac || mac[0] & 1 != 0 || mac == [0; 6] {
                return Err("invalid STA MAC");
            }
            let _ = writeln!(console, "C6 STA MAC {:02x?}", mac);
            let mut sta = [0; 128];
            let mut e = Encoder::new(&mut sta);
            e.bytes(1, ssid)?;
            e.bytes(2, password)?;
            // Require WPA2 or stronger, permit AP's optional PMF.
            e.bytes(9, &[16, 3])?;
            e.bytes(10, &[8, 1])?;
            let sn = e.len;
            let mut config = [0; 144];
            let mut e = Encoder::new(&mut config);
            e.bytes(2, &sta[..sn])?;
            let cn = e.len;
            let mut req = [0; 160];
            let mut e = Encoder::new(&mut req);
            e.bytes(2, &config[..cn])?;
            let rn = e.len;
            let set = link.rpc(284, &req[..rn], timer, console, &mut response);
            wipe(&mut sta);
            wipe(&mut config);
            wipe(&mut req);
            let n = set?;
            result(&response[..n])?;
            let n = link.rpc(282, &[], timer, console, &mut response)?;
            result(&response[..n])?;
            service(&mut link, mac, timer, console)
        })();
        let disconnect = link
            .rpc(283, &[], timer, console, &mut response)
            .and_then(|n| result(&response[..n]));
        let stop = link
            .rpc(281, &[], timer, console, &mut response)
            .and_then(|n| result(&response[..n]));
        let deinit = link
            .rpc(279, &[], timer, console, &mut response)
            .and_then(|n| result(&response[..n]));
        outcome.and(disconnect).and(stop).and(deinit)
    })();
    run
}
fn network(
    link: &mut Link,
    mac: [u8; 6],
    seconds: u32,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
) -> Result<(), &'static str> {
    let started = timer.now_us();
    let xid = started as u32;
    let mut ip = [0; 4];
    let mut offer = None;
    let mut next = 0;
    let mut ready_at = None;
    let mut deadline = 0;
    let mut tx = [0; net::FRAME];
    let (mut packets, mut responses) = (0u32, 0u32);
    loop {
        if console.read_byte() == Some(27) {
            let _ = writeln!(console, "C6 NETWORK stopped by ESC");
            return Ok(());
        }
        let now = timer.now_us();
        if ready_at.is_some() && now >= deadline {
            let _ = writeln!(
                console,
                "C6 NETWORK DONE rx={} replies={}",
                packets, responses
            );
            return Ok(());
        }
        if ready_at.is_none() && now.wrapping_sub(started) > 45_000_000 {
            return Err("association/DHCP timeout");
        }
        if link.disconnected {
            return Err("STA disconnected");
        }
        if link.connected && ip == [0; 4] && now >= next {
            let n = net::dhcp_for_mac(&mut tx, xid, offer, mac);
            link.send(1, &tx[..n], timer)?;
            next = now + 3_000_000;
            let _ = writeln!(
                console,
                "C6 DHCP {}",
                if offer.is_some() {
                    "REQUEST"
                } else {
                    "DISCOVER"
                }
            );
        }
        let rx = unsafe { &mut *core::ptr::addr_of_mut!(RX) };
        let len = link.receive(timer, rx)?;
        let mut at = 0;
        while at < len {
            if rx[at..len].iter().all(|b| *b == 0) {
                break;
            }
            let (interface, payload, used) = hosted::frame(&rx[at..len])?;
            at += used;
            if interface == 3 {
                link.event_packet(payload, console)?;
                continue;
            }
            if interface != 1 {
                continue;
            }
            packets += 1;
            if ip == [0; 4] {
                if let Some((kind, lease)) = net::dhcp_reply_for_mac(payload, xid, mac) {
                    if kind == 2
                        && offer.is_none()
                        && lease.address != [0; 4]
                        && lease.server != [0; 4]
                    {
                        offer = Some(lease);
                        next = now;
                    } else if kind == 5
                        && lease.seconds != 0
                        && offer
                            .is_some_and(|o| o.address == lease.address && o.server == lease.server)
                    {
                        ip = lease.address;
                        ready_at = Some(now);
                        deadline = now + u64::from(seconds.min(lease.seconds)) * 1_000_000;
                        let _ = writeln!(
                            console,
                            "C6 NETWORK READY {}.{}.{}.{} mask={:?} router={:?} seconds={}",
                            ip[0],
                            ip[1],
                            ip[2],
                            ip[3],
                            lease.mask,
                            lease.router,
                            seconds.min(lease.seconds)
                        );
                    } else if kind == 6 {
                        offer = None;
                        next = now + 3_000_000;
                    }
                }
            }
            if let Some(n) = net::reply_for_mac(payload, ip, &mut tx, mac) {
                link.send(1, &tx[..n], timer)?;
                responses += 1;
            }
        }
        timer.delay_us(1000);
    }
}

/// Packet-oriented view of a live C6 link for the shared HTTP server.
pub(crate) struct WifiPort<'a> {
    link: &'a mut Link,
    timer: &'a mut hal::time::Timer,
    address: [u8; 6],
    pending: usize,
    cursor: usize,
}
impl<'a> WifiPort<'a> {
    pub fn new(link: &'a mut Link, address: [u8; 6], timer: &'a mut hal::time::Timer) -> Self {
        Self {
            link,
            timer,
            address,
            pending: 0,
            cursor: 0,
        }
    }
}
impl crate::web_server::Port for WifiPort<'_> {
    fn shares_sdmmc(&self) -> bool {
        true
    }
    fn diagnostics(&self) -> Option<Stats> {
        Some(self.link.stats)
    }

    fn with_timer<R>(&mut self, operation: impl FnOnce(&mut hal::time::Timer) -> R) -> R {
        operation(self.timer)
    }
    fn address(&self) -> [u8; 6] {
        self.address
    }
    fn transmit(&mut self, bytes: &[u8]) -> Result<(), esp32_p4::web::Error> {
        self.link
            .send(1, bytes, self.timer)
            .map_err(|_| esp32_p4::web::Error::Transport)
    }
    fn status(&self) -> [u32; 6] {
        [
            self.link.received,
            self.link.sent,
            u32::from(self.link.sequence),
            u32::from(self.link.connected),
            self.pending as u32,
            self.cursor as u32,
        ]
    }
    fn receive(&mut self, out: &mut [u8]) -> Result<Option<usize>, esp32_p4::web::Error> {
        use esp32_p4::web::Error;
        if self.link.disconnected {
            return Err(Error::Link);
        }
        let rx = unsafe { &mut *core::ptr::addr_of_mut!(RX) };
        if self.cursor >= self.pending {
            self.pending = self
                .link
                .receive(self.timer, rx)
                .map_err(|_| Error::Transport)?;
            self.cursor = 0;
        }
        while self.cursor < self.pending {
            if rx[self.cursor..self.pending].iter().all(|b| *b == 0) {
                self.cursor = self.pending;
                break;
            }
            let (interface, payload) = hosted::stream_frame(&rx[..self.pending], &mut self.cursor)
                .map_err(|_| Error::Frame)?;
            if interface == 1 {
                if payload.len() > out.len() {
                    return Err(Error::Frame);
                }
                out[..payload.len()].copy_from_slice(payload);
                return Ok(Some(payload.len()));
            }
            if interface == 3 {
                let mut proto = hosted::serial(payload).map_err(|_| Error::Frame)?;
                let (mut kind, mut id) = (0, 0);
                while let Some((f, v)) = hosted::field(&mut proto).map_err(|_| Error::Frame)? {
                    match (f, v) {
                        (1, Value::Scalar(v)) => kind = v,
                        (2, Value::Scalar(v)) => id = v,
                        _ => {}
                    }
                }
                if kind == 3 && id == 775 {
                    self.link.connected = true;
                    self.link.disconnected = false;
                }
                if kind == 3 && id == 776 {
                    self.link.connected = false;
                    self.link.disconnected = true;
                    return Err(Error::Link);
                }
            }
        }
        Ok(None)
    }
}
