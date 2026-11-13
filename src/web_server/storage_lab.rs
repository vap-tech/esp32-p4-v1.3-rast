//! Diagnostic-only flat-root FAT32 profiling. No raw writes outside a new file.
use super::*;
use esp32_p4::storage::{files::OpenMode, files::QUANTUM, volume::Error as FsError};
use hal::sdmmc::profile;

/// Counters: reads,writes,readB,writeB,setup_us,data_us,stop_us,ready_us,
/// read_us,write_us,meta_calls,meta_B,meta_us,payload_calls,payload_B,payload_us,
/// CMD13,errors,read512,write512,write1024,write2048,write4096,writeOther.
fn dump(console: &mut hal::usb_serial::Console, tag: &str, before: [u32; profile::N]) {
    let now = profile::snapshot();
    let values: [u32; profile::N] = core::array::from_fn(|i| now[i].wrapping_sub(before[i]));
    let _ = writeln!(console, "SD PROFILE {} {:?}", tag, values);
}
#[derive(Clone, Copy)]
pub(super) struct HttpTrace {
    before: [u32; profile::N],
    start: u64,
    open_us: u64,
    empty_since: u64,
    empty_us: u64,
    write_us: u64,
    close_us: u64,
    calls: u32,
    shared: bool,
}
impl HttpTrace {
    pub const EMPTY: Self = Self {
        before: [0; profile::N],
        start: 0,
        open_us: 0,
        empty_since: 0,
        empty_us: 0,
        write_us: 0,
        close_us: 0,
        calls: 0,
        shared: false,
    };
    pub fn begin(now: u64) -> Self {
        Self {
            before: profile::snapshot(),
            start: now,
            ..Self::EMPTY
        }
    }
    pub fn opened(&mut self, now: u64) {
        self.open_us = now - self.start;
    }
    pub fn shared(&mut self) {
        self.shared = true;
    }
    pub fn empty(&mut self, now: u64) {
        if self.empty_since == 0 {
            self.empty_since = now;
        }
    }
    pub fn available(&mut self, now: u64) {
        if self.empty_since != 0 {
            self.empty_us += now - self.empty_since;
            self.empty_since = 0;
        }
    }
    pub fn wrote(&mut self, us: u64) {
        self.write_us += us;
        self.calls += 1;
    }
    pub fn closed(&mut self, us: u64) {
        self.close_us = us;
    }
    pub fn report(&self, now: u64, slot: usize, console: &mut hal::usb_serial::Console) {
        let _=writeln!(console,"SD HTTP slot={} wall_us={} open_us={} writes={} fs_write_us={} close_us={} rx_empty_us={} shared={}",slot,now-self.start,self.open_us,self.calls,self.write_us,self.close_us,self.empty_us,self.shared);
        dump(console, "HTTP", self.before);
    }
}

pub(super) fn configure<P: Port, D: disk::BlockDevice>(
    partition: &mut disk::Partition<D>,
    network: &RefCell<Network<'_, '_, P>>,
) -> Result<(), web::Error> {
    let mut boot = [0; 512];
    esp32_p4::runtime::run(disk::BlockDevice::read(partition, 0, &mut boot), || {
        network.borrow_mut().io_tick()
    })?
    .map_err(|_| web::Error::Storage)?;
    profile::regions(0, 0, 0);
    let u16at = |i| u16::from_le_bytes([boot[i], boot[i + 1]]) as u64;
    let u32at = |i| u32::from_le_bytes(boot[i..i + 4].try_into().unwrap()) as u64;
    // Other filesystems remain usable; only this region classifier is FAT32-specific.
    if u16at(11) != 512 || u16at(17) != 0 || u16at(22) != 0 || u32at(36) == 0 {
        let _ = writeln!(
            network.borrow_mut().console,
            "SD PROFILE regions unknown (not FAT32)"
        );
        return Ok(());
    }
    let spc = u64::from(boot[13]);
    let reserved = u16at(14);
    let fats = u64::from(boot[16]);
    let fat_sectors = u32at(36);
    let root = u32at(44) & 0xfffffff;
    if spc == 0 || !spc.is_power_of_two() || fats == 0 || root < 2 {
        return Err(web::Error::Storage);
    }
    let relative_data = reserved + fats * fat_sectors;
    let data = partition.start() + relative_data;
    let root_start = data + (root - 2) * spc;
    let mut fat = [0; 512];
    esp32_p4::runtime::run(
        disk::BlockDevice::read(partition, reserved + root * 4 / 512, &mut fat),
        || network.borrow_mut().io_tick(),
    )?
    .map_err(|_| web::Error::Storage)?;
    let at = (root * 4 % 512) as usize;
    let next = u32::from_le_bytes(fat[at..at + 4].try_into().unwrap()) & 0xfffffff;
    let one_cluster = next >= 0xffffff8;
    profile::regions(
        u32::try_from(data).map_err(|_| web::Error::Storage)?,
        u32::try_from(root_start).map_err(|_| web::Error::Storage)?,
        if one_cluster { spc as u32 } else { 0 },
    );
    let _ = writeln!(
        network.borrow_mut().console,
        "SD PROFILE regions data={} root={} sectors={} single_root={}",
        data,
        root_start,
        spc,
        one_cluster
    );
    Ok(())
}

fn wait<F: core::future::Future>(future: F) -> Result<F::Output, web::Error> {
    // Deliberately no network tick or WFI in the SRAM-only measured loop.
    // SDMMC IRQ and 1ms timeout tick still wake the foreground future.
    esp32_p4::runtime::run(future, || Ok::<(), web::Error>(()))
}
fn expected(offset: usize) -> u8 {
    let position = offset % QUANTUM;
    if position < 8 {
        ((offset / QUANTUM) as u64).to_le_bytes()[position]
    } else {
        (position as u8).wrapping_mul(13).wrapping_add(7)
    }
}
pub(super) fn run<P: Port, D: disk::BlockDevice, const N: usize>(
    fs: &mut FileTable<'_, '_, D, N>,
    port: &RefCell<P>,
    console: &mut hal::usb_serial::Console,
    scratch: &mut [u8],
    mib: u32,
) -> Result<(), web::Error> {
    if !(1..=64).contains(&mib) || scratch.len() < QUANTUM {
        return Err(web::Error::Storage);
    }
    let bytes = mib as usize * 1024 * 1024;
    let scratch = &mut scratch[..QUANTUM];
    for (i, b) in scratch.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(13).wrapping_add(7);
    }
    let mut path = http::Text::<32>::new();
    let mut created = None;
    let nonce = port.borrow_mut().now_us() as u32;
    profile::reset();
    let before = profile::snapshot();
    let start = port.borrow_mut().now_us();
    for attempt in 0..64 {
        path = http::Text::new();
        write!(path, "/B{:06X}.BIN", nonce.wrapping_add(attempt) & 0xffffff)
            .map_err(|_| web::Error::Format)?;
        let name = core::str::from_utf8(path.data()).unwrap();
        match wait(fs.open(name, OpenMode::CreateNew))? {
            Ok(h) => {
                created = Some(h);
                break;
            }
            Err(FsError::AlreadyExists) => {}
            Err(_) => return Err(web::Error::Storage),
        }
    }
    let h = created.ok_or(web::Error::Storage)?;
    let _ = writeln!(
        console,
        "SD FILE BENCH WRITE path={} bytes={} source=SRAM no_HTTP",
        core::str::from_utf8(path.data()).unwrap(),
        bytes
    );
    let write_result = (|| {
        for offset in (0..bytes).step_by(QUANTUM) {
            scratch[..8].copy_from_slice(&((offset / QUANTUM) as u64).to_le_bytes());
            let n = wait(fs.write(h, scratch))?.map_err(|_| web::Error::Storage)?;
            if n != QUANTUM {
                return Err(web::Error::Storage);
            }
        }
        wait(fs.close(h))?.map_err(|_| web::Error::Storage)
    })();
    let us = port.borrow_mut().now_us() - start;
    let _ = writeln!(
        console,
        "SD FILE BENCH WRITE bytes={} us={} KiB_s={} result={:?}",
        bytes,
        us,
        bytes as u64 * 1_000_000 / 1024 / us.max(1),
        write_result
    );
    dump(console, "SRAM_WRITE", before);
    if write_result.is_err() {
        if !fs.is_poisoned() {
            let _ = wait(fs.abort(h));
        }
        return write_result;
    }
    let name = core::str::from_utf8(path.data()).unwrap();
    let before = profile::snapshot();
    let start = port.borrow_mut().now_us();
    let h = wait(fs.open(name, OpenMode::Read))?.map_err(|_| web::Error::Storage)?;
    let mut offset = 0;
    while offset < bytes {
        let n = wait(fs.read(h, scratch))?.map_err(|_| web::Error::Storage)?;
        if n == 0
            || scratch[..n]
                .iter()
                .enumerate()
                .any(|(i, b)| *b != expected(offset + i))
        {
            let _ = wait(fs.close(h));
            let _ = wait(fs.remove(name));
            return Err(web::Error::Storage);
        }
        offset += n;
    }
    wait(fs.close(h))?.map_err(|_| web::Error::Storage)?;
    let us = port.borrow_mut().now_us() - start;
    let _ = writeln!(
        console,
        "SD FILE BENCH READ bytes={} us={} KiB_s={} verified=true",
        bytes,
        us,
        bytes as u64 * 1_000_000 / 1024 / us.max(1)
    );
    dump(console, "SRAM_READ", before);
    wait(fs.remove(name))?.map_err(|_| web::Error::Storage)?;
    let _ = writeln!(console, "SD FILE BENCH CLEANUP deleted={}", name);
    Ok(())
}
