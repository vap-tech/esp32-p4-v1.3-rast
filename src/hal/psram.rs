//! AP HEX PSRAM for P4 rev1.x: init at SPLL/12=40 MHz, AXI at SPLL/6=80 MHz.
//! Register settings adapted from ESP-IDF v6.1, Apache-2.0.
use super::time::Timer;
use core::marker::PhantomData;

pub const BASE: usize = 0x8800_0000; // Uncached alias of PSRAM 0x48000000.
pub const RESERVED: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Identity([u16; 4]),
    Memory {
        address: usize,
        expected: u32,
        actual: u32,
    },
}
#[derive(Debug)]
pub struct Info {
    pub mr: [u8; 5],
    pub bytes: usize,
}
pub struct Psram {
    pub info: Info,
    _exclusive: PhantomData<*mut ()>,
}

fn read(addr: usize) -> u32 {
    unsafe { (addr as *const u32).read_volatile() }
}
fn write(addr: usize, value: u32) {
    unsafe { (addr as *mut u32).write_volatile(value) }
}
fn field(addr: usize, mask: u32, shift: u32, value: u32) {
    write(addr, (read(addr) & !mask) | ((value << shift) & mask));
}

// Three resident mask-ROM SPI helpers are used only for PSRAM mode-register
// commands. No ESP-IDF code/runtime is linked. Addresses are P4 eco0..4 ROM ABI.
// Their internal wait is unbounded; bus/PHY power must be established first.
#[repr(C)]
struct RomCommand {
    cmd: u16,
    cmd_len: u16,
    address: *const u32,
    address_len: u32,
    tx: *const u32,
    tx_len: u32,
    rx: *mut u32,
    rx_len: u32,
    dummy: u32,
}
fn transaction(
    command: u16,
    address: u32,
    value: Option<u16>,
    _timer: &mut Timer,
) -> Result<u16, Error> {
    let mut data = u32::from(value.unwrap_or(0));
    let mut conf = RomCommand {
        cmd: command,
        cmd_len: 16,
        address: &address,
        address_len: 32,
        tx: if value.is_some() {
            &data
        } else {
            core::ptr::null()
        },
        tx_len: if value.is_some() { 16 } else { 0 },
        rx: if value.is_none() {
            &mut data
        } else {
            core::ptr::null_mut()
        },
        rx_len: if value.is_none() { 16 } else { 0 },
        dummy: if value.is_none() { 8 } else { 0 },
    };
    unsafe {
        let mode: unsafe extern "C" fn(u32, u32) = core::mem::transmute(0x4fc0_0110usize);
        let config: unsafe extern "C" fn(u32, *mut RomCommand) =
            core::mem::transmute(0x4fc0_0108usize);
        let start: unsafe extern "C" fn(u32, *mut u8, u16, u8, bool) =
            core::mem::transmute(0x4fc0_010cusize);
        mode(3, 7);
        config(3, &mut conf);
        start(
            3,
            (&mut data as *mut u32).cast(),
            if value.is_none() { 2 } else { 0 },
            2,
            false,
        );
    }
    Ok(data as u16)
}

impl Psram {
    /// # Safety
    /// Exclusively own PSRAM/MSPI2/3, dedicated memory pads and PSRAM MMU.
    /// P4 rev1.3, no external mappings in use; code/stack entirely in SRAM.
    pub unsafe fn new(timer: &mut Timer) -> Result<Self, Error> {
        // PSRAM + MSPI PHY domain is supplied by LDO2, 1.8 V.
        let fuse0 = read(0x5012_d04c);
        let fuse1 = read(0x5012_d050);
        let (dref, mul) = if (fuse0 >> 28) != 0 && (fuse1 >> 3) & 7 != 0 {
            (fuse0 >> 28, (fuse1 >> 3) & 7)
        } else {
            (8, 4)
        };
        field(0x5011_51d4, 0xf << 28, 28, dref);
        field(0x5011_51d4, 7 << 23, 23, mul);
        field(0x5011_51d4, 1 << 27, 27, 1);
        field(0x5011_51d0, (7 << 9) | (1 << 14), 0, 0);
        field(0x5011_51d0, (1 << 7) | (1 << 8), 0, (1 << 7) | (1 << 8));
        field(0x5011_51d4, 1 << 27, 27, 0);
        field(0x5011_51d4, 1 << 26, 26, 1);
        field(0x5011_515c, 1 << 24, 24, 1); // MSPI PHY power, also for SPLL.
        timer.delay_us(1_000);
        field(0x500e6014, 0x80000000, 31, 1); // HP_SYS_CLKRST_REG_PSRAM_SYS_CLK_EN
        field(0x500e6030, 0x00004000, 14, 1); // HP_SYS_CLKRST_REG_PSRAM_PLL_CLK_EN
        field(0x500e6030, 0x00008000, 15, 1); // HP_SYS_CLKRST_REG_PSRAM_CORE_CLK_EN
        field(0x500e60c0, 0x00800000, 23, 1); // HP_SYS_CLKRST_REG_RST_EN_DUAL_MSPI_AXI
        field(0x500e60c0, 0x02000000, 25, 1); // HP_SYS_CLKRST_REG_RST_EN_DUAL_MSPI_APB
        field(0x500e60c0, 0x02000000, 25, 0); // HP_SYS_CLKRST_REG_RST_EN_DUAL_MSPI_APB
        field(0x500e60c0, 0x00800000, 23, 0); // HP_SYS_CLKRST_REG_RST_EN_DUAL_MSPI_AXI
        field(0x500e6030, 0x00003000, 12, 2); // HP_SYS_CLKRST_REG_PSRAM_CLK_SRC_SEL
        field(0x500e6030, 0x00ff0000, 16, 0); // HP_SYS_CLKRST_REG_PSRAM_CORE_CLK_DIV_NUM
        field(0x500e121c, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_D_DRV
        field(0x500e1220, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_Q_DRV
        field(0x500e1224, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_WP_DRV
        field(0x500e1228, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_HOLD_DRV
        field(0x500e122c, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ4_DRV
        field(0x500e1230, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ5_DRV
        field(0x500e1234, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ6_DRV
        field(0x500e1238, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ7_DRV
        field(0x500e123c, 0x00018000, 15, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQS_0_DRV
        field(0x500e1240, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_CK_DRV
        field(0x500e1244, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_CS_DRV
        field(0x500e1248, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ8_DRV
        field(0x500e124c, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ9_DRV
        field(0x500e1250, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ10_DRV
        field(0x500e1254, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ11_DRV
        field(0x500e1258, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ12_DRV
        field(0x500e125c, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ13_DRV
        field(0x500e1260, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ14_DRV
        field(0x500e1264, 0x00003000, 12, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQ15_DRV
        field(0x500e1268, 0x00018000, 15, 2); // IOMUX_MSPI_PIN_REG_PSRAM_DQS_1_DRV
        field(0x500e123c, 0x00000001, 0, 1); // IOMUX_MSPI_PIN_REG_PSRAM_DQS_0_XPD
        field(0x500e1268, 0x00000001, 0, 1); // IOMUX_MSPI_PIN_REG_PSRAM_DQS_1_XPD
        field(0x5008e1a0, 0x00000001, 0, 1); // SPI_MEM_S_SMEM_CS_SETUP
        field(0x5008e1a0, 0x00000002, 1, 1); // SPI_MEM_S_SMEM_CS_HOLD
        field(0x5008e1a0, 0x0000007c, 2, 3); // SPI_MEM_S_SMEM_CS_SETUP_TIME
        field(0x5008e1a0, 0x00000f80, 7, 3); // SPI_MEM_S_SMEM_CS_HOLD_TIME
        field(0x5008e1a0, 0x7e000000, 25, 2); // SPI_MEM_S_SMEM_CS_HOLD_DELAY
        field(0x5008e1a0, 0x80000000, 31, 1); // SPI_MEM_S_SMEM_SPLIT_TRANS_EN
        field(0x5008e174, 0x000c0000, 18, 3); // SPI_MEM_S_SMEM_PAGE_SIZE
        write(0x5008e050, (11 << 16) | (5 << 8) | 11); // SPI_MEM_S_SCLK_EQU_SYSCLK
        write(0x5008f014, (11 << 16) | (5 << 8) | 11); // SPI1_MEM_S_CLK_EQU_SYSCLK
        field(0x5008e190, 0x00000020, 5, 1); // SPI_MEM_S_SMEM_DLL_TIMING_CALI
        field(0x5008e180, 0x00000020, 5, 1); // SPI_MEM_S_DLL_TIMING_CALI
        timer.delay_us(1_000);
        let mr01 = transaction(0x4040, 0, None, timer)?;
        let mr23 = transaction(0x4040, 2, None, timer)?;
        let mr48 = transaction(0x4040, 4, None, timer)?;
        let mr8 = transaction(0x4040, 8, None, timer)?;
        let vendor = (mr01 >> 8) as u8 & 31;
        if vendor != 0x0d && vendor != 0x1a {
            return Err(Error::Identity([mr01, mr23, mr48, mr8]));
        }
        // Fixed 10-cycle read latency, five-cycle write, 2K page, x16.
        transaction(0xc0c0, 0, Some((mr01 & 0xff00) | 0x28), timer)?;
        transaction(0xc0c0, 4, Some((mr48 & 0xff1f) | 0x40), timer)?;
        transaction(0xc0c0, 8, Some((mr8 & 0xff00) | 0x4b), timer)?;
        field(0x5008e040, 0x00100000, 20, 1); // SPI_MEM_S_CACHE_SRAM_USR_WCMD
        field(0x5008e04c, 0xf0000000, 28, 15); // SPI_MEM_S_CACHE_SRAM_USR_WR_CMD_BITLEN
        field(0x5008e04c, 0x0000ffff, 0, 32896); // SPI_MEM_S_CACHE_SRAM_USR_WR_CMD_VALUE
        field(0x5008e040, 0x00000020, 5, 1); // SPI_MEM_S_CACHE_SRAM_USR_RCMD
        field(0x5008e048, 0xf0000000, 28, 15); // SPI_MEM_S_CACHE_SRAM_USR_RD_CMD_BITLEN
        field(0x5008e048, 0x0000ffff, 0, 0); // SPI_MEM_S_CACHE_SRAM_USR_RD_CMD_VALUE
        field(0x5008e040, 0x000fc000, 14, 31); // SPI_MEM_S_SRAM_ADDR_BITLEN
        field(0x5008e040, 0x00000001, 0, 1); // SPI_MEM_S_CACHE_USR_SADDR_4BYTE
        field(0x5008e040, 0x00000008, 3, 1); // SPI_MEM_S_USR_WR_SRAM_DUMMY
        field(0x5008e040, 0x0fc00000, 22, 7); // SPI_MEM_S_SRAM_WDUMMY_CYCLELEN
        field(0x5008e040, 0x00000010, 4, 1); // SPI_MEM_S_USR_RD_SRAM_DUMMY
        field(0x5008e040, 0x00000fc0, 6, 17); // SPI_MEM_S_SRAM_RDUMMY_CYCLELEN
        field(0x5008e0d8, 0x00000002, 1, 1); // SPI_MEM_S_SMEM_VAR_DUMMY
        field(0x5008e044, 0x00800000, 23, 1); // SPI_MEM_S_SDUMMY_WOUT
        field(0x5008e0d8, 0x00000008, 3, 0); // SPI_MEM_S_SMEM_DDR_WDAT_SWP
        field(0x5008e0d8, 0x00000004, 2, 0); // SPI_MEM_S_SMEM_DDR_RDAT_SWP
        field(0x5008e0d8, 0x00000001, 0, 1); // SPI_MEM_S_SMEM_DDR_EN
        field(0x5008e040, 0x00200000, 21, 1); // SPI_MEM_S_SRAM_OCT
        field(0x5008e044, 0x00200000, 21, 1); // SPI_MEM_S_SCMD_OCT
        field(0x5008e044, 0x00100000, 20, 1); // SPI_MEM_S_SADDR_OCT
        field(0x5008e044, 0x00080000, 19, 1); // SPI_MEM_S_SDOUT_OCT
        field(0x5008e044, 0x00040000, 18, 1); // SPI_MEM_S_SDIN_OCT
        field(0x5008e044, 0x08000000, 27, 1); // SPI_MEM_S_SDOUT_HEX
        field(0x5008e044, 0x04000000, 26, 1); // SPI_MEM_S_SDIN_HEX
        field(0x5008e03c, 0x00000001, 0, 1); // SPI_MEM_S_AXI_REQ_EN
        field(0x5008e03c, 0x80000000, 31, 0); // SPI_MEM_S_CLOSE_AXI_INF_EN
        field(0x5008e00c, 0x04000000, 26, 1); // SPI_MEM_S_AW_SPLICE_EN
        field(0x5008e00c, 0x02000000, 25, 1); // SPI_MEM_S_AR_SPLICE_EN
                                              // SPLL480 / 6 = 80 MHz, fixed ten-cycle read latency retained.
        write(0x5008e050, (5 << 16) | (2 << 8) | 5);
        write(0x5008f014, (5 << 16) | (2 << 8) | 5);
        // Identity mapping for the first 4 MiB. MMU still participates in
        // uncached alias accesses; L1/L2 caching is bypassed at 0x88000000.
        for page in 0..64u32 {
            write(0x5008_e380, page);
            write(0x5008_e37c, page | (1 << 11) | (1 << 10));
        }
        let density = mr23 as u8 & 7;
        let bytes = match density {
            1 => 4 << 20,
            3 => 8 << 20,
            5 => 16 << 20,
            7 => 32 << 20,
            6 => 64 << 20,
            _ => return Err(Error::Identity([mr01, mr23, mr48, mr8])),
        };
        Ok(Self {
            info: Info {
                mr: [
                    mr01 as u8,
                    (mr01 >> 8) as u8,
                    mr23 as u8,
                    mr48 as u8,
                    mr8 as u8,
                ],
                bytes,
            },
            _exclusive: PhantomData,
        })
    }

    /// Destructive read/write verification of our reserved first 4 MiB.
    pub fn check(&mut self) -> Result<(), Error> {
        for salt in [0x5a6b_7c8d, 0xa594_8372] {
            for offset in (0..RESERVED).step_by(4) {
                write(
                    BASE + offset,
                    (offset as u32).wrapping_mul(0x9e37_79b9) ^ salt,
                );
            }
            for offset in (0..RESERVED).step_by(4) {
                let expected = (offset as u32).wrapping_mul(0x9e37_79b9) ^ salt;
                let actual = read(BASE + offset);
                if actual != expected {
                    return Err(Error::Memory {
                        address: BASE + offset,
                        expected,
                        actual,
                    });
                }
            }
        }
        Ok(())
    }
}
