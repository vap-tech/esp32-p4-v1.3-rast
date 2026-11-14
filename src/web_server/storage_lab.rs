//! Diagnostic-only flat-root FAT32 profiling. No raw writes outside a new file.
use super::*;
use esp32_p4::storage::{files::OpenMode, volume::Error as FsError};
const QUANTUM: usize = hal::sdmmc::MAX_TRANSFER;
use hal::sdmmc::profile;
// This image is single-foreground, D-cache bypassed, no concurrent benchmark.
// Keep the large input off the 64KiB runtime stack and out of Ethernet's DMA pool.
#[cfg(feature = "sd-wide-bench")]
#[repr(align(64))]
struct WideScratch([u8; QUANTUM]);
#[cfg(feature = "sd-wide-bench")]
static mut WIDE_SCRATCH: WideScratch = WideScratch([0; QUANTUM]);

/// Counters: reads,writes,readB,writeB,setup_us,data_us,stop_us,ready_us,
/// read_us,write_us,meta_calls,meta_B,meta_us,payload_calls,payload_B,payload_us,
/// CMD13,errors,read512,write512,write1024,write2048,write4096,writeOther,meta_ready_us,payload_ready_us.
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
    read_us: u64,
    copy_us: u64,
    copy_max_us: u64,
    copy_calls: u32,
    chunks: [u32; 4],
    poll_start: u64,
    losses_start: [u64; 4],
    emac_start: [u32; 6],
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
        read_us: 0,
        copy_us: 0,
        copy_max_us: 0,
        copy_calls: 0,
        chunks: [0; 4],
        poll_start: 0,
        losses_start: [0; 4],
        emac_start: [0; 6],
        close_us: 0,
        calls: 0,
        shared: false,
    };
    pub fn begin(now: u64, poll_start: u64, losses_start: [u64; 4], emac_start: [u32; 6]) -> Self {
        Self {
            before: profile::snapshot(),
            start: now,
            poll_start,
            losses_start,
            emac_start,
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
    pub fn read(&mut self, us: u64) {
        self.read_us += us;
    }
    pub fn copied(&mut self, us: u64, bytes: usize) {
        self.copy_us += us;
        self.copy_max_us = self.copy_max_us.max(us);
        self.copy_calls += 1;
        let bucket = if bytes <= 512 {
            0
        } else if bytes <= 2048 {
            1
        } else if bytes <= 4096 {
            2
        } else {
            3
        };
        self.chunks[bucket] += 1;
    }
    pub fn wrote(&mut self, us: u64) {
        self.write_us += us;
        self.calls += 1;
    }
    pub fn closed(&mut self, us: u64) {
        self.close_us = us;
    }
    pub fn report<P: Port>(&self, now: u64, slot: usize, network: &mut Network<'_, '_, '_, P>) {
        let losses = network.sample_losses();
        let loss_delta: [u64; 4] = core::array::from_fn(|i| losses[i] - self.losses_start[i]);
        let emac = network.port.borrow().wired_stats().unwrap_or([0; 6]);
        let emac_delta: [u32; 6] =
            core::array::from_fn(|i| emac[i].wrapping_sub(self.emac_start[i]));
        let poll_us = network.poll_us - self.poll_start;
        let max_tick_gap = network.max_tick_gap;
        let console = &mut *network.console;
        let _=writeln!(console,"SD HTTP slot={} wall_us={} open_us={} writes={} fs_write_us={} close_us={} rx_wait_us={} tcp_read_us={} psram_copy_us={} copy_max_us={} copy_calls={} copy_sizes={:?} shared={}",slot,now-self.start,self.open_us,self.calls,self.write_us,self.close_us,self.empty_us,self.read_us,self.copy_us,self.copy_max_us,self.copy_calls,self.chunks,self.shared);
        let _=writeln!(console,"HTTP NET poll_us={} max_tick_gap_us={} emac={:?} losses=(rx_buffer,rx_fifo,buffer_counter_wraps,fifo_counter_wraps){:?}",poll_us,max_tick_gap,emac_delta,loss_delta);
        dump(console, "HTTP", self.before);
    }
}

pub(super) fn configure<P: Port, D: disk::BlockDevice>(
    partition: &mut disk::Partition<D>,
    network: &RefCell<Network<'_, '_, '_, P>>,
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
    #[cfg(feature = "sd-wide-bench")] psram: &mut [u8],
) -> Result<(), web::Error> {
    #[cfg(feature = "sd-wide-bench")]
    let scratch = {
        let _ = scratch;
        // SAFETY: sole caller is the serial foreground benchmark; run_mode rejects
        // D-cache ON, no reference escapes and no other code accesses this static.
        unsafe { &mut (*core::ptr::addr_of_mut!(WIDE_SCRATCH)).0 }
    };
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
        wait(fs.sync(h))?.map_err(|_| web::Error::Storage)
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
    // Reuse this file's allocated chain: no writes to unowned card sectors.
    // Keep the CreateNew handle so seek permits overwrite without append.
    #[cfg(feature = "sd-wide-bench")]
    let final_tag = direct_compare(fs, h, port, console, scratch, psram, bytes)?;
    #[cfg(not(feature = "sd-wide-bench"))]
    let final_tag = 0;
    #[cfg(not(feature = "sd-wide-bench"))]
    for chunk in [512, 2048, 8192, QUANTUM]
        .into_iter()
        .enumerate()
        .filter_map(|(i, n)| (i != 3 || n != 8192).then_some(n))
    {
        wait(fs.seek(h, 0))?.map_err(|_| web::Error::Storage)?;
        let before = profile::snapshot();
        let start = port.borrow_mut().now_us();
        let result = (|| {
            for offset in (0..bytes).step_by(QUANTUM) {
                scratch[..8].copy_from_slice(&((offset / QUANTUM) as u64).to_le_bytes());
                for part in scratch.chunks(chunk) {
                    if wait(fs.write(h, part))?.map_err(|_| web::Error::Storage)? != part.len() {
                        return Err(web::Error::Storage);
                    }
                }
            }
            wait(fs.sync(h))?.map_err(|_| web::Error::Storage)
        })();
        let us = port.borrow_mut().now_us() - start;
        let _ = writeln!(
            console,
            "SD FILE BENCH REWRITE chunk={} bytes={} us={} KiB_s={} result={:?}",
            chunk,
            bytes,
            us,
            bytes as u64 * 1_000_000 / 1024 / us.max(1),
            result
        );
        dump(console, "SRAM_REWRITE", before);
        if result.is_err() {
            if !fs.is_poisoned() {
                let _ = wait(fs.abort(h));
            }
            return result;
        }
    }
    wait(fs.close(h))?.map_err(|_| web::Error::Storage)?;
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
                .any(|(i, b)| *b != expected(offset + i) ^ final_tag)
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

#[cfg(feature = "sd-wide-bench")]
fn direct_compare<P: Port, D: disk::BlockDevice, const N: usize>(
    fs: &mut FileTable<'_, '_, D, N>,
    h: esp32_p4::storage::files::FileHandle,
    port: &RefCell<P>,
    console: &mut hal::usb_serial::Console,
    scratch: &mut [u8],
    psram: &mut [u8],
    bytes: usize,
) -> Result<u8, web::Error> {
    use core::sync::atomic::Ordering;
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            profile::DIRECT_TX.store(false, Ordering::Relaxed);
        }
    }
    let _reset = Reset;
    if psram.len() != QUANTUM + 128 {
        return Err(web::Error::Storage);
    }
    psram.fill(0xa5);
    let (head, rest) = psram.split_at_mut(64);
    let (external, tail) = rest.split_at_mut(QUANTUM);
    let mut final_tag = 0;
    // Repeat in reverse order to expose card-state effects. All payloads are 32KiB.
    for (pass, (external_source, direct)) in [
        (false, false),
        (false, true),
        (true, false),
        (true, true),
        (true, true),
        (true, false),
        (false, true),
        (false, false),
    ]
    .into_iter()
    .enumerate()
    {
        let tag = (pass as u8 + 1).wrapping_mul(17);
        let input = if external_source {
            &mut *external
        } else {
            &mut *scratch
        };
        for (i, byte) in input.iter_mut().enumerate() {
            *byte = expected(i) ^ tag;
        }
        wait(fs.seek(h, 0))?.map_err(|_| web::Error::Storage)?;
        profile::DIRECT_BYTES.store(0, Ordering::Relaxed);
        profile::DIRECT_TX.store(direct, Ordering::Relaxed);
        let before = profile::snapshot();
        let start = port.borrow_mut().now_us();
        for offset in (0..bytes).step_by(QUANTUM) {
            let marker = ((offset / QUANTUM) as u64).to_le_bytes();
            for i in 0..8 {
                input[i] = marker[i] ^ tag;
            }
            if wait(fs.write(h, input))?.map_err(|_| web::Error::Storage)? != QUANTUM {
                return Err(web::Error::Storage);
            }
        }
        wait(fs.sync(h))?.map_err(|_| web::Error::Storage)?;
        let us = port.borrow_mut().now_us() - start;
        profile::DIRECT_TX.store(false, Ordering::Relaxed);
        let direct_bytes = profile::DIRECT_BYTES.load(Ordering::Relaxed);
        if direct_bytes as usize != if direct { bytes } else { 0 } {
            return Err(web::Error::Storage);
        }
        let _ = writeln!(
            console,
            "SD DIRECT pass={} source={} mode={} chunk={} bytes={} us={} KiB_s={} direct_bytes={}",
            pass,
            if external_source { "PSRAM" } else { "SRAM" },
            if direct { "direct" } else { "bounce" },
            QUANTUM,
            bytes,
            us,
            bytes as u64 * 1_000_000 / 1024 / us.max(1),
            direct_bytes
        );
        dump(console, "DIRECT", before);
        let _ = writeln!(console, "SD DIRECT HOST {:08x?}", profile::host());
        // Read back after EVERY pass through the unchanged SRAM bounce RX path.
        wait(fs.seek(h, 0))?.map_err(|_| web::Error::Storage)?;
        let mut offset = 0;
        while offset < bytes {
            let n = wait(fs.read_written(h, scratch))?.map_err(|_| web::Error::Storage)?;
            if n == 0
                || scratch[..n]
                    .iter()
                    .enumerate()
                    .any(|(i, b)| *b != (expected(offset + i) ^ tag))
            {
                return Err(web::Error::Storage);
            }
            offset += n;
        }
        if head.iter().chain(tail.iter()).any(|b| *b != 0xa5) {
            return Err(web::Error::Storage);
        }
        let _ = writeln!(
            console,
            "SD DIRECT VERIFIED pass={} source_guards=true",
            pass
        );
        final_tag = tag;
    }
    Ok(final_tag)
}
