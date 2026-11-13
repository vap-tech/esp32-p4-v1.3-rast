//! Native SDMMC slot 0, P4 rev1.x. Internal IDMAC, blocking and IRQ-driven async paths; no SDK runtime.
//! GPIO39..42 D0..3, GPIO43 CLK, GPIO44 CMD. SDSC (CSD v1), SDHC/SDXC (CSD v2).
//! Register sequences adapted from ESP-IDF v6.1 SDMMC LL (Apache-2.0).
use super::time::Timer;
use core::marker::PhantomData;
const BASE: usize = 0x5008_3000;
const SDIO_PLL_HZ: u32 = 200_000_000;
const HOST_DIV: u32 = 2;
const MASK: u64 = (1 << 48) - 1;
/// Maximum bytes per sector transfer (16 sectors), bounded internal SRAM use.
pub const MAX_TRANSFER: usize = 8192;
const DMA_CHUNK: usize = 4096;
#[repr(C, align(64))]
struct DmaBuffer([u32; MAX_TRANSFER / 4]);
// The unsafe constructor grants exclusive controller AND workspace ownership.
// Static SRAM avoids placing an 8 KiB bounce buffer on the small boot stack.
#[link_section = ".dma.fixed"]
static mut DMA_BUFFER: DmaBuffer = DmaBuffer([0; MAX_TRANSFER / 4]);
#[cfg(feature = "wifi")]
static mut COMMAND_LEVELS: [u32; 2] = [u32::MAX, 0];
const RESPONSE: u32 = (1 << 6) | (1 << 8);
const ERRORS: u32 = (1 << 1)
    | (1 << 6)
    | (1 << 7)
    | (1 << 8)
    | (1 << 9)
    | (1 << 10)
    | (1 << 11)
    | (1 << 12)
    | (1 << 13)
    | (1 << 15);
#[derive(Debug)]
pub struct Error {
    pub command: u8,
    pub interrupts: u32,
    pub status: u32,
    pub control: u32,
    pub dma: u32,
    pub pll_state: u32,
    pub response: u32,
}
#[derive(Debug)]
pub struct Info {
    pub ocr: u32,
    pub rca: u16,
    pub cid: [u32; 4],
    pub csd: [u32; 4],
    pub sectors: u64,
    pub width: u8,
    pub clock_hz: u32,
    pub high_speed: bool,
    pub clock_source_hz: u32,
}
pub struct Sdmmc {
    pub info: Info,
    slot: u8,
    divider: u32,
    _exclusive: PhantomData<*mut ()>,
}
unsafe fn read_at(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}
unsafe fn write_at(address: usize, value: u32) {
    unsafe {
        (address as *mut u32).write_volatile(value);
    }
}
fn read(offset: usize) -> u32 {
    unsafe { read_at(BASE + offset) }
}
fn write(offset: usize, value: u32) {
    unsafe {
        write_at(BASE + offset, value);
    }
}
unsafe fn modify_at(address: usize, clear: u32, set: u32) {
    unsafe {
        write_at(address, (read_at(address) & !clear) | set);
    }
}
fn error(command: u8) -> Error {
    Error {
        command,
        interrupts: read(0x44),
        status: read(0x48),
        control: read(0x2c),
        dma: read(0x8c),
        pll_state: unsafe { read_at(0x500e_60bc) },
        response: read(0x30),
    }
}
fn expired(timer: &mut Timer, start: u64, timeout: u64) -> bool {
    timer.now_us().wrapping_sub(start) & MASK >= timeout
}
fn wait_clear(offset: usize, bits: u32, timer: &mut Timer, command: u8) -> Result<(), Error> {
    let start = timer.now_us();
    while read(offset) & bits != 0 {
        if expired(timer, start, 200_000) {
            return Err(error(command));
        }
    }
    Ok(())
}
impl Sdmmc {
    /// Initialize host only, no memory-card commands. Controller ownership includes both slots.
    /// # Safety
    /// Exclusive SDMMC controller/workspace and selected pads, other slot idle.
    unsafe fn host(slot: u8, timer: &mut Timer) -> Result<Self, Error> {
        ASYNC_FAULT.store(false, core::sync::atomic::Ordering::Release);
        ASYNC_ACTIVE.store(false, core::sync::atomic::Ordering::Release);
        unsafe {
            // Board VDDPST_5 / native SD pads are powered from VO4, ext_ldo[4].
            // Software ownership, 3.3V rail bypass, power on. Not LDO2/3.
            modify_at(0x5011_51d8, 7 << 9, (1 << 7) | (1 << 8) | (1 << 14));
            timer.delay_us(10_000);
            modify_at(0x500e_6018, 0, 1 << 14); // SDMMC bus clock.
            modify_at(0x500e_6020, 0, 1 << 3); // IOMUX APB.
            modify_at(0x500e_60ac, 0, 1 << 9); // IOMUX functional clock.
            modify_at(0x5011_104c, 0, 1 << 28);
            modify_at(0x5011_104c, 1 << 28, 0);
            // Dedicated SDIO PLL200, powered independently of SPLL/CPU clocks.
            modify_at(0x5011_515c, 0, 1 << 25);
            write_at(0x5011_50cc, (1 << 20) | (1 << 26) | (1 << 30));
            modify_at(0x5011_1040, 0, 7 << 21);
            // SDK enables this PLL then allows propagation, without using CAL_END.
            // On this rev1.3 chip CAL_END stays zero; card command timeouts below
            // still bound failure to produce a usable clock.
            timer.delay_us(100);
            // PLL200 / 2 = 100MHz cclk_in; SDIO source select, not DSI PLL bits.
            modify_at(0x500e_6034, 1 << 22, (1 << 23) | (1 << 24));
            let fields = (15 << 9) | (15 << 13) | (15 << 17) | (3 << 21) | (3 << 23) | (3 << 25);
            modify_at(
                0x500e_6038,
                fields,
                (1 << 9) | (1 << 17) | (1 << 23) | (7 << 27),
            );
            modify_at(0x500e_6038, 0, 1 << 8);
            modify_at(0x500e_6038, 1 << 8, 0);
            if slot == 0 {
                for pin in 39..=44usize {
                    // Native IOMUX function 0, input enabled, pullup CMD/DAT, drive=3.
                    modify_at(
                        0x500e_1004 + pin * 4,
                        (7 << 12) | (1 << 7) | (1 << 8) | (3 << 10),
                        (1 << 9) | (3 << 10) | if pin == 43 { 0 } else { 1 << 8 },
                    );
                    modify_at(0x500e_0558 + pin * 4, (1 << 10) | (1 << 9), 0);
                }
                // No physical card-detect/write-protect pins. GPIO matrix constants.
                write_at(0x500e_0158 + 126 * 4, 0x3e | (1 << 7)); // CD_N low.
                write_at(0x500e_0158 + 130 * 4, 0x3e | (1 << 7)); // WP low.
            } else {
                for (pin, signal) in [
                    (18usize, 0usize),
                    (19, 1),
                    (14, 2),
                    (15, 3),
                    (16, 4),
                    (17, 5),
                ] {
                    modify_at(
                        0x500e_1004 + pin * 4,
                        (7 << 12) | (1 << 7) | (1 << 8) | (3 << 10),
                        (1 << 12) | (1 << 9) | (3 << 10) | if pin == 18 { 0 } else { 1 << 8 },
                    );
                    modify_at(0x500e_0074 + pin * 4, 1 << 2, 0);
                    // Peripheral output-enable, not a permanently driven GPIO output.
                    write_at(0x500e_0558 + pin * 4, signal as u32);
                    write_at(0x500e_0158 + signal * 4, pin as u32 | (1 << 7));
                    modify_at(0x500e_0020, 0, 1 << pin);
                }
                write_at(0x500e_0158 + 127 * 4, 0x3e | (1 << 7));
                write_at(0x500e_0158 + 131 * 4, 0x3e | (1 << 7));
            }
        }
        write(0, 7); // Controller, FIFO, internal DMA reset.
        wait_clear(0, 7, timer, 0xfe)?;
        write(0x24, 0); // No CPU interrupts.
        write(0x44, u32::MAX);
        write(0x14, u32::MAX); // Response/data timeout, also bounded in software.
        write(0x18, 0); // Start 1-bit native SD.
        write(0x4c, (15 << 16) | 15); // FIFO thresholds, 16 words.
        let this = Self {
            info: Info {
                ocr: 0,
                rca: 0,
                cid: [0; 4],
                csd: [0; 4],
                sectors: 0,
                width: 1,
                clock_hz: 400_000,
                high_speed: false,
                clock_source_hz: SDIO_PLL_HZ,
            },
            slot,
            divider: 125,
            _exclusive: PhantomData,
        };
        Ok(this)
    }
    /// # Safety
    /// Initialization exclusively owns SDMMC controller, GPIO39..44 and DMA workspace.
    /// Idle slot1 handles may coexist only if all operations share one exclusive Timer. Card uses 3.3V;
    /// LDO4 supplies the SD pad domain at the 3.3V rail; no UHS voltage switch.
    pub unsafe fn new(timer: &mut Timer) -> Result<Self, Error> {
        let mut this = unsafe { Self::host(0, timer) }?;
        this.set_clock(125, timer)?;
        timer.delay_us(2_000);
        this.command(0, 0, 1 << 15, timer)?;
        let version2 = match this.command(8, 0x1aa, RESPONSE, timer) {
            Ok(r7) if r7[0] & 0xfff == 0x1aa => true,
            Ok(_) => return Err(error(8)),
            // Legacy SD v1 cards do not answer CMD8. Only a response timeout
            // permits fallback; CRC and other transport failures remain errors.
            Err(e) if e.interrupts & ERRORS == 1 << 8 => false,
            Err(e) => return Err(e),
        };
        let start = timer.now_us();
        loop {
            this.command(55, 0, RESPONSE, timer)?;
            let ocr = this.command(
                41,
                0x00ff_8000 | if version2 { 1 << 30 } else { 0 },
                1 << 6,
                timer,
            )?[0];
            if ocr & (1 << 31) != 0 {
                this.info.ocr = ocr;
                break;
            }
            if expired(timer, start, 1_500_000) {
                return Err(error(41));
            }
            timer.delay_us(10_000);
        }
        this.info.cid = this.command(2, 0, RESPONSE | (1 << 7), timer)?;
        this.info.rca = (this.command(3, 0, RESPONSE, timer)?[0] >> 16) as u16;
        let rca = u32::from(this.info.rca) << 16;
        this.info.csd = this.command(9, rca, RESPONSE | (1 << 7), timer)?;
        let block_addressed = this.info.ocr & (1 << 30) != 0;
        if this.info.csd[3] >> 30 != u32::from(block_addressed) {
            return Err(error(9));
        }
        this.info.sectors = decode_capacity(this.info.csd).ok_or_else(|| error(9))?;
        this.command(7, rca, RESPONSE, timer)?;
        wait_clear(0x48, 1 << 9, timer, 7)?;
        if !block_addressed {
            let r1 = this.command(16, 512, RESPONSE, timer)?[0];
            if r1 & 0xfdff_e008 != 0 {
                return Err(error(16));
            }
        }
        // Verify one-bit transport first before negotiating four data lines.
        Ok(this)
    }
    pub fn set_four_bit(&mut self, timer: &mut Timer) -> Result<(), Error> {
        let r1 = self.app_command(6, 2, timer)?[0];
        if r1 & 0xfdff_e008 != 0 {
            return Err(error(6));
        }
        write(0x18, read(0x18) | (1 << self.slot));
        self.info.width = 4;
        self.set_clock(2, timer)?; // PLL200 / 2 / (2*2) = 25MHz, default-speed SD.
        Ok(())
    }
    /// Select 1/4-bit from SCR and High Speed only when CMD6 declares it.
    /// One SDIO PLL200 source: 400kHz probing, 25MHz default / 50MHz High Speed.
    pub fn configure_bus(&mut self, timer: &mut Timer) -> Result<(), Error> {
        let scr = self.read_scr(timer)?;
        if scr[0] >> 4 != 0 {
            return Err(error(51));
        }
        if scr[1] & 4 != 0 {
            self.set_four_bit(timer)?;
        } else {
            self.set_clock(2, timer)?;
        }
        if !supports_switch(self.info.csd, scr) {
            return Ok(());
        }
        let start = timer.now_us();
        loop {
            let check = self.check_speed(timer)?;
            if !high_speed_supported(&check) {
                return Ok(());
            }
            if check[17] == 0 || u16::from_be_bytes([check[28], check[29]]) & 2 == 0 {
                break;
            }
            if expired(timer, start, 500_000) {
                return Err(error(6));
            }
            timer.delay_us(1_000);
        }
        let mut status = [0u8; 64];
        self.data_command(6, 0x80ff_fff1, &mut status, timer)?;
        if status[16] & 15 != 1 {
            return Err(error(6));
        }
        self.info.high_speed = true;
        self.set_clock(1, timer)?;
        // Confirm command-data transport after changing both card mode and clock.
        let verify = self.read_scr(timer);
        if verify.as_ref().is_ok_and(|actual| *actual == scr) {
            return Ok(());
        }
        // High Speed cards also accept a slower clock. Keep accurate mode state.
        self.set_clock(2, timer)?;
        match verify {
            Err(e) => Err(e),
            Ok(_) => Err(error(51)),
        }
    }
    fn app_command(&mut self, cmd: u8, arg: u32, timer: &mut Timer) -> Result<[u32; 4], Error> {
        self.app_prefix(timer)?;
        self.command(cmd, arg, RESPONSE, timer)
    }
    fn activate(&mut self, timer: &mut Timer) -> Result<(), Error> {
        let slot = u32::from(self.slot);
        let bit = 1 << slot;
        let width = if self.info.width == 4 { bit } else { 0 };
        if read(0x18) & bit != width {
            write(0x18, (read(0x18) & !bit) | width);
        }
        let shift = slot * 8;
        if read(0x10) & bit == 0
            || (read(8) >> shift) & 255 != self.divider
            || (read(0xc) >> (slot * 2)) & 3 != slot
        {
            self.set_clock(self.divider, timer)?;
        }
        Ok(())
    }
    fn set_clock(&mut self, divider: u32, timer: &mut Timer) -> Result<(), Error> {
        let slot = u32::from(self.slot);
        write(0x10, read(0x10) & !(1 << slot));
        self.update_clock(timer)?;
        write(
            8,
            (read(8) & !(255 << (slot * 8))) | (divider << (slot * 8)),
        );
        write(0xc, (read(0xc) & !(3 << (slot * 2))) | (slot << (slot * 2)));
        self.update_clock(timer)?;
        write(0x10, read(0x10) | (1 << slot));
        self.update_clock(timer)?;
        self.divider = divider;
        self.info.clock_hz = card_clock_hz(divider);
        Ok(())
    }
    fn update_clock(&mut self, timer: &mut Timer) -> Result<(), Error> {
        write(0x44, u32::MAX);
        write(
            0x2c,
            (1 << 31) | (1 << 29) | (1 << 21) | (1 << 13) | (u32::from(self.slot) << 16),
        );
        wait_clear(0x2c, 1 << 31, timer, 0xfd)?;
        if read(0x44) & (1 << 12) != 0 {
            return Err(error(0xfd));
        }
        Ok(())
    }
    fn command(
        &mut self,
        cmd: u8,
        arg: u32,
        flags: u32,
        timer: &mut Timer,
    ) -> Result<[u32; 4], Error> {
        if ASYNC_ACTIVE.load(core::sync::atomic::Ordering::Acquire)
            || ASYNC_FAULT.load(core::sync::atomic::Ordering::Acquire)
        {
            return Err(error(cmd));
        }
        self.activate(timer)?;
        wait_clear(0x2c, 1 << 31, timer, cmd)?;
        write(0x44, u32::MAX);
        write(0x28, arg);
        write(
            0x2c,
            (1 << 31)
                | (1 << 29)
                | if cmd == 12 { 1 << 14 } else { 1 << 13 }
                | flags
                | (u32::from(self.slot) << 16)
                | u32::from(cmd),
        );
        let start = timer.now_us();
        #[cfg(feature = "wifi")]
        if self.slot == 1 {
            unsafe {
                COMMAND_LEVELS = [u32::MAX, 0];
            }
        }
        loop {
            #[cfg(feature = "wifi")]
            if self.slot == 1 {
                let value = unsafe { read_at(0x500e_003c) };
                unsafe {
                    COMMAND_LEVELS[0] &= value;
                    COMMAND_LEVELS[1] |= value;
                }
            }
            let ints = read(0x44);
            if ints & ERRORS != 0 {
                return Err(error(cmd));
            }
            if ints & (1 << 2) != 0 {
                break;
            }
            if expired(timer, start, 300_000) {
                return Err(error(cmd));
            }
        }
        Ok(core::array::from_fn(|i| read(0x30 + i * 4)))
    }
    /// Read one sector through SDMMC's own IDMAC (not shared DW-GDMA).
    pub fn read_sector(
        &mut self,
        lba: u32,
        out: &mut [u8; 512],
        timer: &mut Timer,
    ) -> Result<(), Error> {
        self.read_sectors(lba, out, timer)
    }
    /// Read 1..=16 consecutive 512-byte sectors. Multi-block uses CMD18 and
    /// controller-generated CMD12; no CMD23 support is required from the card.
    pub fn read_sectors(
        &mut self,
        lba: u32,
        out: &mut [u8],
        timer: &mut Timer,
    ) -> Result<(), Error> {
        let cmd = if out.len() == 512 { 17 } else { 18 };
        let address = transfer_argument(lba, out.len(), self.info.sectors, self.info.ocr)
            .ok_or_else(|| error(cmd))?;
        self.data_command(cmd, address, out, timer)
    }
    /// Read-only diagnostic: publish tail canaries, dirty RX cache lines, then
    /// use the normal DMA path. Returns (tail intact, dirty line observed).
    #[inline(never)]
    pub fn read_poisoned(
        &mut self,
        lba: u32,
        out: &mut [u8],
        salt: u32,
        timer: &mut Timer,
    ) -> Result<(bool, bool), Error> {
        if out.is_empty() || out.len() > MAX_TRANSFER || !out.len().is_multiple_of(512) {
            return Err(error(17));
        }
        let ptr = core::ptr::addr_of_mut!(DMA_BUFFER).cast::<u32>();
        for i in 0..MAX_TRANSFER / 4 {
            unsafe {
                ptr.add(i).write_volatile(salt);
            }
        }
        sync_cache(ptr as usize, MAX_TRANSFER, 8, timer)?;
        for i in 0..out.len() / 4 {
            unsafe {
                ptr.add(i).write_volatile(!salt);
            }
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
        let physical = unsafe { ((ptr as usize + 0x4000_0000) as *const u32).read_volatile() };
        let dirty = physical != !salt;
        self.read_sectors(lba, out, timer)?;
        let mut tail_ok = true;
        for i in out.len() / 4..MAX_TRANSFER / 4 {
            tail_ok &= unsafe {
                ((ptr as usize + 0x4000_0000) as *const u32)
                    .add(i)
                    .read_volatile()
            } == salt;
        }
        Ok((tail_ok, dirty))
    }
    /// Write a sector and wait for TRANSFER/READY. No filesystem allocation.
    pub fn write_sector(
        &mut self,
        lba: u32,
        data: &[u8; 512],
        timer: &mut Timer,
    ) -> Result<(), Error> {
        self.write_sectors(lba, data, timer)
    }
    /// Write 1..=16 consecutive sectors using CMD24/CMD25 and auto CMD12.
    pub fn write_sectors(&mut self, lba: u32, data: &[u8], timer: &mut Timer) -> Result<(), Error> {
        let cmd = if data.len() == 512 { 24 } else { 25 };
        let address = transfer_argument(lba, data.len(), self.info.sectors, self.info.ocr)
            .ok_or_else(|| error(cmd))?;
        // data_command uses a separate output slice only for read transfers.
        self.transfer(cmd, address, &mut [], Some(data), data.len(), timer)
    }
    fn data_command(
        &mut self,
        cmd: u8,
        argument: u32,
        out: &mut [u8],
        timer: &mut Timer,
    ) -> Result<(), Error> {
        let len = out.len();
        self.transfer(cmd, argument, out, None, len, timer)
    }
    fn transfer(
        &mut self,
        cmd: u8,
        argument: u32,
        out: &mut [u8],
        input: Option<&[u8]>,
        len: usize,
        timer: &mut Timer,
    ) -> Result<(), Error> {
        self.activate(timer)?;
        #[repr(C, align(64))]
        struct Descriptors([[u32; 16]; MAX_TRANSFER / DMA_CHUNK]);
        if len == 0 || len > MAX_TRANSFER || !len.is_multiple_of(4) {
            return Err(error(cmd));
        }
        let writing = input.is_some();
        let multi = cmd == 18 || cmd == 25;
        let data = unsafe { &mut *core::ptr::addr_of_mut!(DMA_BUFFER) };
        if let Some(input) = input {
            for (i, chunk) in input.as_chunks::<4>().0.iter().enumerate() {
                data.0[i] = u32::from_le_bytes(*chunk);
            }
        }
        let mut desc = Descriptors([[0; 16]; MAX_TRANSFER / DMA_CHUNK]);
        let desc_addr = desc.0.as_mut_ptr() as usize;
        let data_addr = data.0.as_mut_ptr() as usize;
        let count = len.div_ceil(DMA_CHUNK);
        for i in 0..count {
            let last = i + 1 == count;
            desc.0[i][0] = (1 << 31) | (1 << 4) // OWN, chained addresses.
                | if i == 0 { 1 << 3 } else { 0 }
                | if last { 1 << 2 } else { 1 << 1 };
            desc.0[i][1] = (len - i * DMA_CHUNK).min(DMA_CHUNK) as u32;
            desc.0[i][2] = (data_addr + i * DMA_CHUNK) as u32;
            desc.0[i][3] = if last {
                0
            } else {
                (desc_addr + (i + 1) * 64) as u32
            };
        }
        sync_cache(desc_addr, count * 64, 8, timer)?;
        sync_cache(data_addr, len.next_multiple_of(64), 8, timer)?;
        write(0, 6);
        wait_clear(0, 6, timer, cmd)?;
        write(0x80, 1);
        wait_clear(0x80, 1, timer, cmd)?;
        write(0x88, desc_addr as u32);
        write(0x8c, u32::MAX);
        write(0x90, 0x1f); // IDMAC completion latches need masks set; global CPU IRQ stays off.
        write(
            0x1c,
            if multi || (cmd == 53 && argument & (1 << 27) != 0) {
                512
            } else {
                len as u32
            },
        );
        write(0x20, len as u32);
        write(0, (1 << 25) | (1 << 5));
        write(0x80, (1 << 7) | (1 << 1) | (2 << 8));
        let result = (|| {
            let r1 = self.command(
                cmd,
                argument,
                RESPONSE | (1 << 9) | (u32::from(writing) << 10) | (u32::from(multi) << 12),
                timer,
            )?[0];
            if r1 & if cmd == 53 { 0xcb00 } else { 0xfdff_e008 } != 0 {
                return Err(error(cmd));
            }
            let start = timer.now_us();
            loop {
                if read(0x44) & ERRORS != 0 || read(0x8c) & ((1 << 2) | (1 << 5)) != 0 {
                    return Err(error(cmd));
                }
                if read(0x44) & (1 << 3) != 0
                    && (!multi || read(0x44) & (1 << 14) != 0)
                    && read(0x8c) & if writing { 1 } else { 2 } != 0
                {
                    break;
                }
                if expired(timer, start, 500_000) {
                    return Err(error(cmd));
                }
            }
            Ok(())
        })();
        // Never release stack DMA memory until IDMAC has stopped.
        write(0, 4);
        write(0x80, 0);
        if wait_clear(0, 4, timer, cmd).is_err() {
            panic!("SDMMC DMA stop timeout");
        }
        if result.is_err() && multi {
            // Best effort abort after failed stream. CMD12 bypasses wait-data.
            let _ = self.command(12, 0, RESPONSE, timer);
        }
        result?;
        sync_cache(desc_addr, count * 64, 1, timer)?;
        sync_cache(data_addr, len.next_multiple_of(64), 1, timer)?;
        for i in 0..count {
            if unsafe { desc.0[i].as_ptr().read_volatile() } & ((1 << 31) | (1 << 30)) != 0 {
                return Err(error(cmd));
            }
        }
        for i in 0..if writing { 0 } else { out.len() / 4 } {
            let value = unsafe { data.0.as_ptr().add(i).read_volatile() };
            out[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        write(0x44, u32::MAX);
        if (writing || multi) && cmd != 53 {
            let start = timer.now_us();
            loop {
                let r1 = self.command(13, u32::from(self.info.rca) << 16, RESPONSE, timer)?[0];
                if r1 & 0xfdff_e008 != 0 {
                    return Err(error(13));
                }
                if r1 & (1 << 8) != 0 && (r1 >> 9) & 15 == 4 {
                    break;
                }
                if expired(timer, start, 1_000_000) {
                    return Err(error(13));
                }
                timer.delay_us(1_000);
            }
        }
        Ok(())
    }
    /// SCR: eight-byte big-endian SD Configuration Register, no mode switch.
    pub fn read_scr(&mut self, timer: &mut Timer) -> Result<[u8; 8], Error> {
        self.app_prefix(timer)?;
        let mut bytes = [0; 8];
        self.data_command(51, 0, &mut bytes, timer)?;
        Ok(bytes)
    }
    /// ACMD13, 64-byte SD Status. Does not change speed or bus width.
    pub fn read_sd_status(&mut self, timer: &mut Timer) -> Result<[u8; 64], Error> {
        self.app_prefix(timer)?;
        let mut bytes = [0; 64];
        self.data_command(13, 0, &mut bytes, timer)?;
        Ok(bytes)
    }
    /// CMD6 mode 0: check High Speed function; other groups stay unchanged.
    /// The high switch bit is deliberately zero: this NEVER enables High Speed.
    pub fn check_speed(&mut self, timer: &mut Timer) -> Result<[u8; 64], Error> {
        let mut bytes = [0; 64];
        self.data_command(6, 0x00ff_fff1, &mut bytes, timer)?;
        Ok(bytes)
    }
    fn app_prefix(&mut self, timer: &mut Timer) -> Result<(), Error> {
        let r1 = self.command(55, u32::from(self.info.rca) << 16, RESPONSE, timer)?[0];
        if r1 & 0xfdff_e008 != 0 || r1 & (1 << 5) == 0 {
            return Err(error(55));
        }
        Ok(())
    }
    pub fn registers(&self) -> [u32; 8] {
        [
            read(0),
            read(8),
            read(0x10),
            read(0x18),
            read(0x44),
            read(0x48),
            read(0x800),
            read(0x2c),
        ]
    }
}

fn sync_cache(address: usize, len: usize, operation: u32, timer: &mut Timer) -> Result<(), Error> {
    const CACHE: usize = 0x3ff1_0000;
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack));
        write_at(CACHE + 0x9c, 1 << 4); // L1 DCache, internal SRAM.
        write_at(CACHE + 0xa0, address as u32);
        write_at(CACHE + 0xa4, len as u32);
        // P4 ROM patch repeats writeback/invalidate; ranges are 64B isolated.
        for _ in 0..if operation == 8 { 2 } else { 1 } {
            write_at(CACHE + 0x98, operation);
            let start = timer.now_us();
            while read_at(CACHE + 0x98) & 16 == 0 {
                if expired(timer, start, 200_000) {
                    return Err(error(0xfc));
                }
            }
        }
        core::arch::asm!("fence rw, rw", options(nostack));
    }
    Ok(())
}

fn transfer_argument(lba: u32, len: usize, sectors: u64, ocr: u32) -> Option<u32> {
    if len == 0
        || len > MAX_TRANSFER
        || !len.is_multiple_of(512)
        || u64::from(lba) + (len / 512) as u64 > sectors
    {
        return None;
    }
    if ocr & (1 << 30) == 0 {
        sector_argument(lba.checked_add((len / 512 - 1) as u32)?, ocr)?;
    }
    sector_argument(lba, ocr)
}

fn sector_argument(lba: u32, ocr: u32) -> Option<u32> {
    if ocr & (1 << 30) != 0 {
        Some(lba)
    } else {
        lba.checked_mul(512)
    }
}

fn decode_capacity(csd: [u32; 4]) -> Option<u64> {
    match csd[3] >> 30 {
        0 => {
            let read_bl_len = (csd[2] >> 16) & 15; // bits 83:80
            if !(9..=11).contains(&read_bl_len) {
                return None;
            }
            let c_size = ((csd[2] & 0x3ff) << 2) | (csd[1] >> 30); // 73:62
            let mult = (csd[1] >> 15) & 7; // 49:47
            Some((u64::from(c_size) + 1) << (mult + 2 + read_bl_len - 9))
        }
        1 => {
            let c_size = ((csd[2] & 0x3f) << 16) | (csd[1] >> 16);
            Some((u64::from(c_size) + 1) * 1024)
        }
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multi_block_ranges_and_addressing() {
        assert_eq!(transfer_argument(7, 1024, 9, 0), Some(3584));
        assert_eq!(transfer_argument(7, 1024, 8, 0), None);
        assert_eq!(transfer_argument(0, 0, 100, 0), None);
        assert_eq!(transfer_argument(0, 513, 100, 0), None);
        assert_eq!(transfer_argument(0, MAX_TRANSFER + 512, 100, 0), None);
        assert_eq!(
            transfer_argument(0xfffffff0, 8192, 1u64 << 32, 1 << 30),
            Some(0xfffffff0)
        );
        assert_eq!(transfer_argument(u32::MAX, 1024, 1u64 << 32, 1 << 30), None);
        assert_eq!(transfer_argument(8_388_607, 1024, 10_000_000, 0), None);
    }
    #[test]
    fn sdsc_capacity_and_byte_address_boundaries() {
        // CSD v1: C_SIZE=4095, MULT=7, READ_BL_LEN=10 => 2 GiB.
        let csd = [0, (3 << 30) | (7 << 15), (10 << 16) | 1023, 0];
        assert_eq!(decode_capacity(csd), Some(4_194_304));
        assert_eq!(sector_argument(1, 0x80ff8000), Some(512));
        assert_eq!(sector_argument(4_194_303, 0x80ff8000), Some(0x7ffffe00));
        assert_eq!(sector_argument(8_388_607, 0), Some(0xfffffe00));
        assert_eq!(sector_argument(8_388_608, 0), None);
        assert_eq!(sector_argument(u32::MAX, 1 << 30), Some(u32::MAX));
        // Actual SDSC capture: 998244352 bytes, independently readable FAT32.
        assert_eq!(
            decode_capacity([373293211, 4005298079, 1398375351, 8323122]),
            Some(1_949_696)
        );
        // Small CSD v1: 1024 native 512B blocks.
        assert_eq!(decode_capacity([0, 3 << 30, (9 << 16) | 63, 0]), Some(1024));
        assert_eq!(decode_capacity([0, 0, 0, 2 << 30]), None);
    }
    #[test]
    fn dedicated_source_covers_probing_default_and_high_speed_exactly() {
        assert_eq!(card_clock_hz(125), 400_000);
        assert_eq!(card_clock_hz(2), 25_000_000);
        assert_eq!(card_clock_hz(1), 50_000_000);
    }
    #[test]
    fn high_speed_requires_scr_command_class_and_function_one() {
        let csd = [171966509, 1001357184, 1532559360, 1074659378];
        let scr = [0x02, 0x35, 0x80, 0x01, 0, 0, 0, 0];
        assert!(supports_switch(csd, scr));
        let mut no_switch = csd;
        no_switch[2] &= !(1 << 30); // CCC SWITCH = response bit 94.
        assert!(!supports_switch(no_switch, scr));
        let mut old = scr;
        old[0] = 0; // SD 1.0 has no CMD6 switch-function support.
        assert!(!supports_switch(csd, old));
        let mut status = [0u8; 64];
        status[12] = 0x80;
        status[13] = 1;
        assert!(!high_speed_supported(&status)); // Default + no-change only.
        status[13] = 3;
        assert!(high_speed_supported(&status)); // Actual card response: 0x8003.
    }
    #[test]
    fn capacity_matches_card_csd_and_reader_size() {
        // Actual register capture; laptop reader independently reports 8011120640B.
        let csd = [171966509, 1001357184, 1532559360, 1074659378];
        assert_eq!(decode_capacity(csd), Some(15_646_720));
        assert_eq!(decode_capacity(csd).unwrap() * 512, 8_011_120_640);
        assert_eq!(decode_capacity([0; 4]), None); // Invalid READ_BL_LEN.
        assert_eq!(
            decode_capacity([0, 0xffff0000, 0x3f, 1 << 30]),
            Some(4_294_967_296)
        );
    }
}

fn supports_switch(csd: [u32; 4], scr: [u8; 8]) -> bool {
    scr[0] >> 4 == 0 && scr[0] & 15 >= 1 && (csd[2] >> 20) & (1 << 10) != 0
}
fn high_speed_supported(status: &[u8; 64]) -> bool {
    // Reserved/no-change bit 15 is not a speed. Only access function 1 matters.
    u16::from_be_bytes([status[12], status[13]]) & 2 != 0
}

const fn card_clock_hz(divider: u32) -> u32 {
    SDIO_PLL_HZ / HOST_DIV / if divider == 0 { 1 } else { 2 * divider }
}

/// SDIO function access for the board's C6. Owns the whole host until dropped.
/// No storage commands (CMD17/24 etc.) are sent to the SD card slot.
#[cfg(feature = "wifi")]
pub struct Sdio {
    host: Sdmmc,
    pub ocr: u32,
    pub cccr: [u8; 24],
}
#[cfg(feature = "wifi")]
impl Sdio {
    /// # Safety
    /// Initialization exclusively owns SDMMC, GPIO14..19, C6 reset GPIO54 and DMA workspace.
    /// An idle slot0 handle may survive this reset; serialize both handles through one Timer.
    pub unsafe fn new(timer: &mut Timer, reset_active_high: bool) -> Result<Self, Error> {
        // Our module: GPIO54 active LOW, verified by receiving the C6 boot event. No Flash writes.
        unsafe {
            modify_at(0x500e_6020, 0, 1 << 3);
            modify_at(0x500e_60ac, 0, 1 << 9);
            write_at(0x500e_0034, 1 << 22); // GPIO54 output disable.
            write_at(
                if reset_active_high {
                    0x500e_0014
                } else {
                    0x500e_0018
                },
                1 << 22,
            ); // Assert reset.
            modify_at(
                0x500e_1004 + 54 * 4,
                (7 << 12) | (1 << 7) | (1 << 8),
                (1 << 12) | (1 << 9),
            );
            write_at(0x500e_0558 + 54 * 4, 256 | (1 << 10));
            write_at(0x500e_0030, 1 << 22);
            timer.delay_us(20_000);
            write_at(
                if reset_active_high {
                    0x500e_0018
                } else {
                    0x500e_0014
                },
                1 << 22,
            ); // Release reset.
        }
        timer.delay_us(1_000_000);
        let mut host = unsafe { Sdmmc::host(1, timer) }?;
        host.set_clock(125, timer)?;
        host.command(0, 0, 1 << 15, timer)?;
        let start = timer.now_us();
        let mut ocr = loop {
            match host.command(5, 0, 1 << 6, timer) {
                Ok(response) => break response[0],
                Err(e) if e.interrupts & ERRORS == 1 << 8 && !expired(timer, start, 5_000_000) => {
                    timer.delay_us(100_000)
                }
                Err(e) => return Err(e),
            }
        };
        if ocr & 0x7000_0000 == 0 {
            return Err(error(5));
        }
        let start = timer.now_us();
        loop {
            ocr = host.command(5, ocr & 0x00ff_8000, 1 << 6, timer)?[0];
            if ocr & (1 << 31) != 0 {
                break;
            }
            if expired(timer, start, 1_000_000) {
                return Err(error(5));
            }
            timer.delay_us(1_000);
        }
        host.info.rca = (host.command(3, 0, RESPONSE, timer)?[0] >> 16) as u16;
        host.command(7, u32::from(host.info.rca) << 16, RESPONSE, timer)?;
        let mut this = Self {
            host,
            ocr,
            cccr: [0; 24],
        };
        for i in 0..this.cccr.len() {
            this.cccr[i] = this.read_byte(0, i as u32, timer)?;
        }
        Ok(this)
    }
    pub fn read_byte(
        &mut self,
        function: u8,
        address: u32,
        timer: &mut Timer,
    ) -> Result<u8, Error> {
        let arg = cmd52_arg(false, function, address, 0).ok_or_else(|| error(52))?;
        let r5 = self.host.command(52, arg, RESPONSE, timer)?[0];
        if r5 & 0xcb00 != 0 {
            return Err(error(52));
        }
        Ok(r5 as u8)
    }
    pub fn write_byte(
        &mut self,
        function: u8,
        address: u32,
        data: u8,
        timer: &mut Timer,
    ) -> Result<(), Error> {
        let arg = cmd52_arg(true, function, address, data).ok_or_else(|| error(52))?;
        let r5 = self.host.command(52, arg, RESPONSE, timer)?[0];
        if r5 & 0xcb00 != 0 {
            return Err(error(52));
        }
        Ok(())
    }
    /// One function-1 CMD53 byte-mode transaction, or a reproducible CMD52 control.
    pub fn read_register(&mut self, address: u32, timer: &mut Timer) -> Result<u32, Error> {
        if cfg!(feature = "sdio-reg-cmd52") {
            return self.read_register_cmd52(address, timer);
        }
        let arg = cmd53_bytes_arg(1, address, 4).ok_or_else(|| error(53))?;
        let mut bytes = [0; 4];
        self.host.transfer(53, arg, &mut bytes, None, 4, timer)?;
        Ok(u32::from_le_bytes(bytes))
    }
    fn read_register_cmd52(&mut self, address: u32, timer: &mut Timer) -> Result<u32, Error> {
        let mut bytes = [0; 4];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = self.read_byte(1, address + i as u32, timer)?;
        }
        Ok(u32::from_le_bytes(bytes))
    }
    pub fn enable(&mut self, timer: &mut Timer) -> Result<(), Error> {
        self.write_byte(0, 2, self.cccr[2] | 2, timer)?;
        let start = timer.now_us();
        while self.read_byte(0, 3, timer)? & 2 == 0 {
            if expired(timer, start, 1_000_000) {
                return Err(error(52));
            }
        }
        for (address, value) in [(0x10, 0), (0x11, 2), (0x110, 0), (0x111, 2)] {
            self.write_byte(0, address, value, timer)?;
        }
        self.write_byte(0, 4, 3, timer)?;
        Ok(())
    }
    pub fn receive(
        &mut self,
        address: u32,
        out: &mut [u8],
        timer: &mut Timer,
    ) -> Result<(), Error> {
        if address > 0x1ffff
            || out.is_empty()
            || out.len() > 16384
            || !out.len().is_multiple_of(512)
        {
            return Err(error(53));
        }
        for (index, part) in out.chunks_mut(MAX_TRANSFER).enumerate() {
            let len = part.len();
            let address = address + (index * MAX_TRANSFER) as u32;
            if address > 0x1ffff {
                return Err(error(53));
            }
            let arg = (1 << 28) | (1 << 27) | (1 << 26) | (address << 9) | (len as u32 / 512);
            self.host.transfer(53, arg, part, None, len, timer)?;
        }
        Ok(())
    }
    pub fn four_bit_25mhz(&mut self, timer: &mut Timer) -> Result<(), Error> {
        let width = self.read_byte(0, 7, timer)?;
        self.write_byte(0, 7, (width & !3) | 2, timer)?;
        write(0x18, read(0x18) | (1 << 1));
        self.host.info.width = 4;
        self.host.set_clock(2, timer)
    }
    pub fn transmit(&mut self, address: u32, data: &[u8], timer: &mut Timer) -> Result<(), Error> {
        let len = data.len();
        if address > 0x1ffff || len == 0 || len > MAX_TRANSFER || !len.is_multiple_of(512) {
            return Err(error(53));
        }
        let arg =
            (1 << 31) | (1 << 28) | (1 << 27) | (1 << 26) | (address << 9) | (len as u32 / 512);
        self.host.transfer(53, arg, &mut [], Some(data), len, timer)
    }
    pub fn registers(&self) -> [u32; 8] {
        self.host.registers()
    }
}
#[cfg(any(feature = "wifi", test))]
fn cmd52_arg(writing: bool, function: u8, address: u32, data: u8) -> Option<u32> {
    if function > 7 || address > 0x1ffff {
        return None;
    }
    Some(
        (u32::from(writing) << 31) | (u32::from(function) << 28) | (address << 9) | u32::from(data),
    )
}

#[cfg(feature = "wifi")]
pub fn sdio_debug() -> ([u32; 14], [u32; 6], [u32; 6], [u32; 6]) {
    let mut all = u32::MAX;
    let mut any = 0;
    for _ in 0..4096 {
        let v = unsafe { read_at(0x500e_003c) };
        all &= v;
        any |= v;
    }
    (
        [
            read(8),
            read(0xc),
            read(0x10),
            read(0x18),
            read(0x50),
            all,
            any,
            unsafe { read_at(0x500e_0020) },
            unsafe { read_at(0x500e_0040) },
            unsafe { read_at(0x500e_0030 - 4) },
            unsafe { read_at(0x500e_1004 + 54 * 4) },
            unsafe { read_at(0x500e_0558 + 54 * 4) },
            unsafe { COMMAND_LEVELS[0] },
            unsafe { COMMAND_LEVELS[1] },
        ],
        core::array::from_fn(|i| unsafe { read_at(0x500e_0558 + (14 + i) * 4) }),
        core::array::from_fn(|i| unsafe { read_at(0x500e_0158 + i * 4) }),
        core::array::from_fn(|i| unsafe { read_at(0x500e_1004 + (14 + i) * 4) }),
    )
}

/// CMD53 byte-mode, incrementing address. Count zero encodes 512 bytes.
#[cfg(any(feature = "wifi", test))]
fn cmd53_bytes_arg(function: u8, address: u32, len: usize) -> Option<u32> {
    if function > 7
        || len == 0
        || len > 512
        || !len.is_multiple_of(4)
        || address.checked_add(len as u32 - 1)? > 0x1ffff
    {
        return None;
    }
    Some((u32::from(function) << 28) | (1 << 26) | (address << 9) | (len as u32 & 0x1ff))
}
#[cfg(test)]
mod sdio_tests {
    use super::*;
    #[test]
    fn byte_mode_register_command_and_bounds() {
        assert_eq!(cmd53_bytes_arg(1, 0x60, 4), Some(0x1400c004));
        assert_eq!(cmd53_bytes_arg(1, 0x44, 4), Some(0x14008804));
        assert_eq!(cmd53_bytes_arg(1, 0, 512), Some(0x14000000));
        assert_eq!(cmd53_bytes_arg(1, 0x1fffd, 4), None);
        assert_eq!(cmd53_bytes_arg(8, 0, 4), None);
        for len in [0, 1, 3, 513] {
            assert_eq!(cmd53_bytes_arg(1, 0, len), None);
        }
    }
    #[test]
    fn direct_command_wire_fields_and_bounds() {
        assert_eq!(cmd52_arg(false, 0, 3, 0), Some(3 << 9));
        assert_eq!(cmd52_arg(true, 1, 0x8c, 1), Some(0x90011801));
        assert_eq!(cmd52_arg(false, 8, 0, 0), None);
        assert_eq!(cmd52_arg(false, 1, 0x20000, 0), None);
    }
}

/// Resumable DMA operation. Descriptors/data are controller-owned static SRAM;
/// user slices are copied only before DMA starts / after it has stopped.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AsyncPhase {
    Acquire,
    FifoReset,
    DmaReset,
    Data,
    Stop,
    CardReadyStart,
    CardReady,
    Done,
}
#[repr(C, align(64))]
struct AsyncDescriptors([[u32; 16]; MAX_TRANSFER / DMA_CHUNK]);
static mut ASYNC_DESCRIPTORS: AsyncDescriptors =
    AsyncDescriptors([[0; 16]; MAX_TRANSFER / DMA_CHUNK]);
static ASYNC_ACTIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static ASYNC_FAULT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Diagnostic counters, microsecond totals wrap after ~71 minutes; reset per test.
#[cfg(feature = "diagnostics")]
pub mod profile {
    use core::sync::atomic::{AtomicU32, Ordering};
    pub const N: usize = 24;
    static COUNTERS: [AtomicU32; N] = [const { AtomicU32::new(0) }; N];
    static DATA_START: AtomicU32 = AtomicU32::new(0);
    static ROOT_START: AtomicU32 = AtomicU32::new(0);
    static ROOT_END: AtomicU32 = AtomicU32::new(0);
    pub fn add(i: usize, n: u64) {
        COUNTERS[i].fetch_add(n as u32, Ordering::Relaxed);
    }
    pub fn snapshot() -> [u32; N] {
        core::array::from_fn(|i| COUNTERS[i].load(Ordering::Relaxed))
    }
    pub fn reset() {
        for c in &COUNTERS {
            c.store(0, Ordering::Relaxed);
        }
    }
    pub fn regions(data_start: u32, root_start: u32, root_sectors: u32) {
        DATA_START.store(data_start, Ordering::Relaxed);
        ROOT_START.store(root_start, Ordering::Relaxed);
        ROOT_END.store(root_start.saturating_add(root_sectors), Ordering::Relaxed);
    }
    pub fn metadata(lba: u32, len: usize) -> bool {
        let end = u64::from(lba) + (len / 512) as u64;
        let data = DATA_START.load(Ordering::Relaxed);
        data != 0
            && (end <= u64::from(data)
                || lba >= ROOT_START.load(Ordering::Relaxed)
                    && end <= u64::from(ROOT_END.load(Ordering::Relaxed)))
    }
}
pub struct Request {
    cmd: u8,
    arg: u32,
    len: usize,
    writing: bool,
    phase: AsyncPhase,
    deadline: u64,
    transaction_deadline: u64,
    seen: u32,
    claimed: bool,
    #[cfg(feature = "diagnostics")]
    profile_start: u64,
    #[cfg(feature = "diagnostics")]
    profile_mark: u64,
    #[cfg(feature = "diagnostics")]
    profile_metadata: bool,
}
impl Request {
    fn new(cmd: u8, arg: u32, len: usize, writing: bool) -> Self {
        Self {
            cmd,
            arg,
            len,
            writing,
            phase: AsyncPhase::Acquire,
            deadline: 0,
            transaction_deadline: 0,
            seen: 0,
            claimed: false,
            #[cfg(feature = "diagnostics")]
            profile_start: 0,
            #[cfg(feature = "diagnostics")]
            profile_mark: 0,
            #[cfg(feature = "diagnostics")]
            profile_metadata: false,
        }
    }
    #[cfg(feature = "diagnostics")]
    fn profile_phase(&mut self, index: usize, timer: &mut Timer) {
        let now = timer.now_us();
        profile::add(index, now - self.profile_mark);
        self.profile_mark = now;
    }
    fn issue(&mut self, cmd: u8, arg: u32, flags: u32, timer: &mut Timer) {
        super::interrupt::events();
        self.seen = 0;
        write(0x44, u32::MAX);
        write(0x24, super::interrupt::SD_MASK);
        write(0, read(0) | (1 << 4));
        write(0x28, arg);
        write(
            0x2c,
            (1 << 31) | (1 << 29) | (1 << 13) | flags | u32::from(cmd),
        );
        self.deadline = timer.now_us() + 500_000;
    }
    fn stop(&mut self) {
        write(0x24, 0);
        write(0, 0);
        write(0, 4);
        write(0x80, 0);
        write(0x90, 0);
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        if self.claimed {
            // Cancellation never exposes user memory to DMA. Quarantine this
            // controller until explicit host/card initialization after abort.
            ASYNC_FAULT.store(true, core::sync::atomic::Ordering::Release);
            self.stop();
            write(0, 7);
            for _ in 0..100000 {
                if read(0) & 7 == 0 {
                    ASYNC_ACTIVE.store(false, core::sync::atomic::Ordering::Release);
                    return;
                }
            }
            panic!("SDMMC async abort did not quiesce");
        }
    }
}
impl Sdmmc {
    pub fn request_read(&self, lba: u32, len: usize) -> Result<Request, Error> {
        let cmd = if len == 512 { 17 } else { 18 };
        let arg = transfer_argument(lba, len, self.info.sectors, self.info.ocr)
            .ok_or_else(|| error(cmd))?;
        Ok(Request::new(cmd, arg, len, false))
    }
    pub fn request_write(&self, lba: u32, len: usize) -> Result<Request, Error> {
        let cmd = if len == 512 { 24 } else { 25 };
        let arg = transfer_argument(lba, len, self.info.sectors, self.info.ocr)
            .ok_or_else(|| error(cmd))?;
        Ok(Request::new(cmd, arg, len, true))
    }
    /// Advance without a hardware wait loop. Each pending wait is woken by the
    /// SDMMC ISR, with the scoped timer tick providing timeout/reset progress.
    /// Slot0 memory-card transfers only; host/slot initialization stays synchronous.
    pub fn poll_request(
        &mut self,
        req: &mut Request,
        cx: &mut core::task::Context<'_>,
        timer: &mut Timer,
        out: &mut [u8],
        input: Option<&[u8]>,
    ) -> core::task::Poll<Result<(), Error>> {
        use core::{sync::atomic::Ordering, task::Poll};
        super::interrupt::register(cx.waker());
        let result = (|| -> Result<bool, Error> {
            if self.slot != 0
                || ASYNC_FAULT.load(Ordering::Acquire)
                || !super::interrupt::is_active()
            {
                return Err(error(req.cmd));
            }
            for _ in 0..8 {
                // CMD13 retries must not extend the total busy timeout forever.
                if req.claimed && timer.now_us() > req.transaction_deadline {
                    return Err(error(req.cmd));
                }
                if req.phase != AsyncPhase::Acquire
                    && req.phase != AsyncPhase::Done
                    && timer.now_us() > req.deadline
                {
                    return Err(error(req.cmd));
                }
                match req.phase {
                    AsyncPhase::Acquire => {
                        if ASYNC_ACTIVE
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_err()
                        {
                            return Ok(false);
                        }
                        req.claimed = true;
                        #[cfg(feature = "diagnostics")]
                        {
                            req.profile_start = timer.now_us();
                            req.profile_mark = req.profile_start;
                            let lba = if self.info.ocr & (1 << 30) != 0 {
                                req.arg
                            } else {
                                req.arg / 512
                            };
                            req.profile_metadata = profile::metadata(lba, req.len);
                        }
                        req.transaction_deadline = timer.now_us() + 2_000_000;
                        self.activate(timer)?;
                        if req.writing {
                            let b = input
                                .filter(|b| b.len() == req.len)
                                .ok_or_else(|| error(req.cmd))?;
                            for (i, b) in b.as_chunks::<4>().0.iter().enumerate() {
                                unsafe {
                                    (*core::ptr::addr_of_mut!(DMA_BUFFER)).0[i] =
                                        u32::from_le_bytes(*b);
                                }
                            }
                        } else if out.len() != req.len {
                            return Err(error(req.cmd));
                        }
                        let desc = core::ptr::addr_of_mut!(ASYNC_DESCRIPTORS);
                        let data = core::ptr::addr_of_mut!(DMA_BUFFER) as usize;
                        let count = req.len.div_ceil(DMA_CHUNK);
                        unsafe {
                            (*desc).0 = [[0; 16]; MAX_TRANSFER / DMA_CHUNK];
                            for i in 0..count {
                                let last = i + 1 == count;
                                (*desc).0[i][0] = (1 << 31)
                                    | (1 << 4)
                                    | if i == 0 { 1 << 3 } else { 0 }
                                    | if last { 1 << 2 } else { 1 << 1 };
                                (*desc).0[i][1] = (req.len - i * DMA_CHUNK).min(DMA_CHUNK) as u32;
                                (*desc).0[i][2] = (data + i * DMA_CHUNK) as u32;
                                (*desc).0[i][3] = if last {
                                    0
                                } else {
                                    desc as usize as u32 + (i as u32 + 1) * 64
                                };
                            }
                        }
                        sync_cache(desc as usize, count * 64, 8, timer)?;
                        sync_cache(data, req.len.next_multiple_of(64), 8, timer)?;
                        write(0, 6);
                        req.phase = AsyncPhase::FifoReset;
                        req.deadline = timer.now_us() + 500_000;
                    }
                    AsyncPhase::FifoReset => {
                        if read(0) & 6 != 0 {
                            return Ok(false);
                        }
                        write(0x80, 1);
                        req.phase = AsyncPhase::DmaReset;
                    }
                    AsyncPhase::DmaReset => {
                        if read(0x80) & 1 != 0 {
                            return Ok(false);
                        }
                        write(0x88, core::ptr::addr_of_mut!(ASYNC_DESCRIPTORS) as u32);
                        write(0x8c, u32::MAX);
                        write(0x90, 0);
                        write(0x1c, 512);
                        write(0x20, req.len as u32);
                        write(0, (1 << 25) | (1 << 5) | (1 << 4));
                        write(0x80, (1 << 7) | (1 << 1) | (2 << 8));
                        #[cfg(feature = "diagnostics")]
                        req.profile_phase(4, timer);
                        req.issue(
                            req.cmd,
                            req.arg,
                            RESPONSE
                                | (1 << 9)
                                | (u32::from(req.writing) << 10)
                                | (u32::from(matches!(req.cmd, 18 | 25)) << 12),
                            timer,
                        );
                        req.phase = AsyncPhase::Data;
                        return Ok(false);
                    }
                    AsyncPhase::Data => {
                        req.seen |= super::interrupt::events() | read(0x44);
                        if req.seen & ERRORS != 0 || read(0x8c) & ((1 << 2) | (1 << 5)) != 0 {
                            return Err(error(req.cmd));
                        }
                        if req.seen & (1 << 2) != 0 && read(0x30) & 0xfdffe008 != 0 {
                            return Err(error(req.cmd));
                        }
                        if req.seen & (1 << 3) == 0
                            || matches!(req.cmd, 18 | 25) && req.seen & (1 << 14) == 0
                        {
                            return Ok(false);
                        }
                        let desc = core::ptr::addr_of!(ASYNC_DESCRIPTORS);
                        if unsafe {
                            (*desc).0[req.len.div_ceil(DMA_CHUNK) - 1]
                                .as_ptr()
                                .read_volatile()
                        } & (1 << 31)
                            != 0
                        {
                            return Ok(false);
                        }
                        #[cfg(feature = "diagnostics")]
                        req.profile_phase(5, timer);
                        req.stop();
                        req.phase = AsyncPhase::Stop;
                    }
                    AsyncPhase::Stop => {
                        if read(0) & 4 != 0 {
                            return Ok(false);
                        }
                        let desc = core::ptr::addr_of!(ASYNC_DESCRIPTORS);
                        sync_cache(desc as usize, req.len.div_ceil(DMA_CHUNK) * 64, 1, timer)?;
                        sync_cache(
                            core::ptr::addr_of!(DMA_BUFFER) as usize,
                            req.len.next_multiple_of(64),
                            1,
                            timer,
                        )?;
                        for i in 0..req.len.div_ceil(DMA_CHUNK) {
                            if unsafe { (*desc).0[i].as_ptr().read_volatile() }
                                & ((1 << 31) | (1 << 30))
                                != 0
                            {
                                return Err(error(req.cmd));
                            }
                        }
                        if req.writing {
                            #[cfg(feature = "diagnostics")]
                            req.profile_phase(6, timer);
                            req.phase = AsyncPhase::CardReadyStart;
                        } else {
                            for (i, b) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                                let v = unsafe {
                                    (*core::ptr::addr_of!(DMA_BUFFER))
                                        .0
                                        .as_ptr()
                                        .add(i)
                                        .read_volatile()
                                };
                                *b = v.to_le_bytes();
                            }
                            #[cfg(feature = "diagnostics")]
                            req.profile_phase(6, timer);
                            req.phase = AsyncPhase::Done;
                        }
                    }
                    AsyncPhase::CardReadyStart => {
                        #[cfg(feature = "diagnostics")]
                        profile::add(16, 1);
                        req.issue(13, u32::from(self.info.rca) << 16, RESPONSE, timer);
                        req.phase = AsyncPhase::CardReady;
                        return Ok(false);
                    }
                    AsyncPhase::CardReady => {
                        req.seen |= super::interrupt::events() | read(0x44);
                        if req.seen & ERRORS != 0 {
                            return Err(error(13));
                        }
                        if req.seen & (1 << 2) == 0 {
                            return Ok(false);
                        }
                        let r = read(0x30);
                        if r & 0xfdffe008 != 0 {
                            return Err(error(13));
                        }
                        if r & (1 << 8) != 0 && (r >> 9) & 15 == 4 {
                            #[cfg(feature = "diagnostics")]
                            req.profile_phase(7, timer);
                            req.phase = AsyncPhase::Done;
                        } else {
                            req.phase = AsyncPhase::CardReadyStart;
                            return Ok(false);
                        }
                    }
                    AsyncPhase::Done => {
                        #[cfg(feature = "diagnostics")]
                        {
                            let us = timer.now_us() - req.profile_start;
                            profile::add(usize::from(req.writing), 1);
                            profile::add(2 + usize::from(req.writing), req.len as u64);
                            profile::add(8 + usize::from(req.writing), us);
                            let class = if req.profile_metadata { 10 } else { 13 };
                            profile::add(class, 1);
                            profile::add(class + 1, req.len as u64);
                            profile::add(class + 2, us);
                            if req.len == 512 {
                                profile::add(18 + usize::from(req.writing), 1);
                            }
                            if req.writing {
                                profile::add(
                                    match req.len {
                                        512 => 19,
                                        1024 => 20,
                                        2048 => 21,
                                        4096 => 22,
                                        _ => 23,
                                    },
                                    u64::from(req.len != 512),
                                );
                            }
                        }
                        write(0x24, 0);
                        write(0, read(0) & !(1 << 4));
                        req.claimed = false;
                        ASYNC_ACTIVE.store(false, Ordering::Release);
                        return Ok(true);
                    }
                }
            }
            cx.waker().wake_by_ref();
            Ok(false)
        })();
        match result {
            Ok(true) => Poll::Ready(Ok(())),
            Ok(false) => Poll::Pending,
            Err(mut e) => {
                #[cfg(feature = "diagnostics")]
                profile::add(17, 1);
                e.interrupts |= req.seen;
                Poll::Ready(Err(e))
            }
        }
    }
}
