//! Native SDMMC slot 0, P4 rev1.x. Polling internal IDMAC, no interrupts or SDK runtime.
//! GPIO39..42 D0..3, GPIO43 CLK, GPIO44 CMD. SDSC (CSD v1), SDHC/SDXC (CSD v2).
//! Register sequences adapted from ESP-IDF v6.1 SDMMC LL (Apache-2.0).
use super::time::Timer;
use core::marker::PhantomData;
const BASE: usize = 0x5008_3000;
const SDIO_PLL_HZ: u32 = 200_000_000;
const HOST_DIV: u32 = 2;
const MASK: u64 = (1 << 48) - 1;
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
    /// # Safety
    /// Exclusive SDMMC controller/slot0 and GPIO39..44 ownership. Card uses 3.3V;
    /// LDO4 supplies the SD pad domain at the 3.3V rail; no UHS voltage switch.
    pub unsafe fn new(timer: &mut Timer) -> Result<Self, Error> {
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
            write_at(0x500e_0154 + 126 * 4, 0x38 | (1 << 7)); // CD_N low.
            write_at(0x500e_0154 + 130 * 4, 0x38 | (1 << 7)); // WP low.
        }
        write(0, 7); // Controller, FIFO, internal DMA reset.
        wait_clear(0, 7, timer, 0xfe)?;
        write(0x24, 0); // No CPU interrupts.
        write(0x44, u32::MAX);
        write(0x14, u32::MAX); // Response/data timeout, also bounded in software.
        write(0x18, 0); // Start 1-bit native SD.
        write(0x4c, (15 << 16) | 15); // FIFO thresholds, 16 words.
        let mut this = Self {
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
            _exclusive: PhantomData,
        };
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
        write(0x18, 1);
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
        self.data_command(6, 0x80ff_fff1, &mut status, false, timer)?;
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
    fn set_clock(&mut self, divider: u32, timer: &mut Timer) -> Result<(), Error> {
        write(0x10, 0);
        self.update_clock(timer)?;
        write(8, divider);
        write(0xc, 0);
        self.update_clock(timer)?;
        write(0x10, 1);
        self.update_clock(timer)?;
        self.info.clock_hz = card_clock_hz(divider);
        Ok(())
    }
    fn update_clock(&mut self, timer: &mut Timer) -> Result<(), Error> {
        write(0x44, u32::MAX);
        write(0x2c, (1 << 31) | (1 << 29) | (1 << 21) | (1 << 13));
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
        wait_clear(0x2c, 1 << 31, timer, cmd)?;
        write(0x44, u32::MAX);
        write(0x28, arg);
        write(
            0x2c,
            (1 << 31) | (1 << 29) | (1 << 13) | flags | u32::from(cmd),
        );
        let start = timer.now_us();
        loop {
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
        if u64::from(lba) >= self.info.sectors {
            return Err(error(17));
        }
        let address = sector_argument(lba, self.info.ocr).ok_or_else(|| error(17))?;
        self.data_command(17, address, out, false, timer)
    }
    /// Write one logical sector and wait until the card returns to TRANSFER/READY.
    /// No filesystem mapping or allocation is performed.
    pub fn write_sector(
        &mut self,
        lba: u32,
        data: &[u8; 512],
        timer: &mut Timer,
    ) -> Result<(), Error> {
        let mut copy = *data;
        if u64::from(lba) >= self.info.sectors {
            return Err(error(24));
        }
        let address = sector_argument(lba, self.info.ocr).ok_or_else(|| error(24))?;
        self.data_command(24, address, &mut copy, true, timer)
    }
    fn data_command(
        &mut self,
        cmd: u8,
        argument: u32,
        out: &mut [u8],
        writing: bool,
        timer: &mut Timer,
    ) -> Result<(), Error> {
        #[repr(C, align(64))]
        struct Aligned<const N: usize>([u32; N]);
        if out.is_empty() || out.len() > 512 || !out.len().is_multiple_of(4) {
            return Err(error(cmd));
        }
        let mut data = Aligned([0xa5a5_a5a5; 128]);
        if writing {
            for (i, chunk) in out.as_chunks::<4>().0.iter().enumerate() {
                data.0[i] = u32::from_le_bytes(*chunk);
            }
        }
        let mut desc = Aligned([0u32; 16]);
        desc.0[0] = (1 << 31) | (1 << 3) | (1 << 2); // OWN, FS, LS.
        desc.0[1] = out.len() as u32;
        desc.0[2] = data.0.as_mut_ptr() as u32;
        let desc_addr = desc.0.as_mut_ptr() as usize;
        let data_addr = data.0.as_mut_ptr() as usize;
        sync_cache(desc_addr, 64, 8, timer)?;
        sync_cache(data_addr, 512, 8, timer)?;
        write(0, 6);
        wait_clear(0, 6, timer, cmd)?;
        write(0x80, 1);
        wait_clear(0x80, 1, timer, cmd)?;
        write(0x88, desc_addr as u32);
        write(0x8c, u32::MAX);
        write(0x90, 0x1f); // IDMAC completion latches need masks set; global CPU IRQ stays off.
        write(0x1c, out.len() as u32);
        write(0x20, out.len() as u32);
        write(0, (1 << 25) | (1 << 5));
        write(0x80, (1 << 7) | (1 << 1) | (2 << 8));
        let result = (|| {
            let r1 = self.command(
                cmd,
                argument,
                RESPONSE | (1 << 9) | (u32::from(writing) << 10),
                timer,
            )?[0];
            if r1 & 0xfdff_e008 != 0 {
                return Err(error(cmd));
            }
            let start = timer.now_us();
            loop {
                if read(0x44) & ERRORS != 0 || read(0x8c) & ((1 << 2) | (1 << 5)) != 0 {
                    return Err(error(cmd));
                }
                if read(0x44) & (1 << 3) != 0 && read(0x8c) & if writing { 1 } else { 2 } != 0 {
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
        result?;
        sync_cache(desc_addr, 64, 1, timer)?;
        sync_cache(data_addr, 512, 1, timer)?;
        if unsafe { desc.0.as_ptr().read_volatile() } & ((1 << 31) | (1 << 30)) != 0 {
            return Err(error(cmd));
        }
        for i in 0..if writing { 0 } else { out.len() / 4 } {
            let value = unsafe { data.0.as_ptr().add(i).read_volatile() };
            out[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        write(0x44, u32::MAX);
        if writing {
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
        self.data_command(51, 0, &mut bytes, false, timer)?;
        Ok(bytes)
    }
    /// ACMD13, 64-byte SD Status. Does not change speed or bus width.
    pub fn read_sd_status(&mut self, timer: &mut Timer) -> Result<[u8; 64], Error> {
        self.app_prefix(timer)?;
        let mut bytes = [0; 64];
        self.data_command(13, 0, &mut bytes, false, timer)?;
        Ok(bytes)
    }
    /// CMD6 mode 0: check High Speed function; other groups stay unchanged.
    /// The high switch bit is deliberately zero: this NEVER enables High Speed.
    pub fn check_speed(&mut self, timer: &mut Timer) -> Result<[u8; 64], Error> {
        let mut bytes = [0; 64];
        self.data_command(6, 0x00ff_fff1, &mut bytes, false, timer)?;
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
        write_at(CACHE + 0x98, operation);
        let start = timer.now_us();
        while read_at(CACHE + 0x98) & operation != 0 {
            if expired(timer, start, 200_000) {
                return Err(error(0xfc));
            }
        }
        core::arch::asm!("fence rw, rw", options(nostack));
    }
    Ok(())
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
