// Register sequences and LDO calibration adapted from ESP-IDF v6.1:
// components/esp_hal_lcd/{mipi_dsi_hal.c,esp32p4/include/hal/mipi_dsi*_ll.h}
// components/esp_hal_pmu/esp32p4/include/hal/ldo_ll.h
// SPDX-FileCopyrightText: 2023-2025 Espressif Systems (Shanghai) CO LTD
// SPDX-License-Identifier: Apache-2.0
//! Minimal MIPI DSI host for P4 rev 1.x, 2 lanes, 550 Mbps, VC0.
//! DCS, hardware patterns and one-buffer RGB565 DMA scanout; no interrupts.
use super::time::Timer;
use core::marker::PhantomData;

const HOST: usize = 0x500a_0000;
const BRIDGE: usize = 0x500a_0800;
const CLOCK: usize = 0x500e_6000;
const LDO: usize = 0x5011_51c0; // VO3 = unit 2 = ext_ldo[1], not ext_ldo[2].

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    PllTimeout,
    LanesTimeout,
    FifoTimeout,
    PacketTooLong,
    ReadTimeout,
    ReceiveFifo,
    LinkRecovery,
    ReadResponse { host0: u32, host1: u32 },
}

#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub video_enabled: bool,
    pub version: u32,
    pub phy: u32,
    pub fifo: u32,
    pub errors0: u32,
    pub errors1: u32,
    pub video: u32,
    pub clock: u32,
    pub ref20: u32,
    pub ldo: u32,
    pub ldo_analog: u32,
    pub color_coding: u32,
    pub hline: u32,
    pub lp_command_size: u32,
    pub clock_dividers: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    Vertical,
    Horizontal,
    Ber,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb565,
    Rgb888,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneRate {
    Mbps550,
    Mbps650,
    Mbps750,
}
impl LaneRate {
    pub fn mbps(self) -> u32 {
        match self {
            Self::Mbps550 => 550,
            Self::Mbps650 => 650,
            Self::Mbps750 => 750,
        }
    }
}

pub struct Dsi {
    _exclusive: PhantomData<*mut ()>,
    rate: LaneRate,
    format: PixelFormat,
    observed_errors: [u32; 2],
    scanout: bool,
}

impl Dsi {
    /// Initialize the DSI PHY and command interface; leave video disabled.
    ///
    /// # Safety
    /// Own DSI host/bridge and LDO3 exclusively. P4 rev 1.x, XTAL 40 MHz,
    /// ROM-configured SPLL 480 MHz. No concurrent clock reconfiguration.
    /// Board must wire LDO3 to the 2.5-V DSI PHY, as on JC1060P470C.
    pub unsafe fn new(timer: &mut Timer) -> Result<Self, Error> {
        let mut dsi = Self {
            _exclusive: PhantomData,
            rate: LaneRate::Mbps550,
            format: PixelFormat::Rgb565,
            observed_errors: [0; 2],
            scanout: false,
        };
        enable_ldo3();
        timer.delay_us(5_000);
        modify(CLOCK + 0x18, 0, 1 << 12); // DSI APB clock.
        modify(CLOCK + 0xc0, 0, 1 << 26);
        modify(CLOCK + 0xc0, 1 << 26, 0);
        // On rev 1.x the config AND PLL reference share PLL_F20M.
        modify(CLOCK + 0x28, 0xff << 16, 23 << 16); // SPLL / 24.
        modify(CLOCK + 0x2c, 0, 1 << 8); // REF_20M clock gate.
        modify(CLOCK + 0x38, 3 << 30, 0); // PHY source = PLL_F20M.
        modify(CLOCK + 0x3c, 0, 3); // Config + PLL reference clock enable.
        write(HOST + 0x04, 0);
        write(HOST + 0xa0, 0);
        write(HOST + 0xa4, 1); // Two data lanes.
        write(HOST + 0x04, 1);
        write(HOST + 0xa0, 0x0d); // Shutdown released, clock enabled, force PLL.
        write(HOST + 0xa0, 0x0f); // Release PHY reset.
                                  // 20 MHz * M(110) / N(4) = 550 Mbps; range 550..599 = 0x17.
        dsi.phy_write(0x44, 0x17 << 1);
        dsi.phy_write(0x19, 0x30);
        dsi.phy_write(0x17, 3);
        dsi.phy_write(0x18, 109 & 0x1f);
        dsi.phy_write(0x18, 0x80 | (109 >> 5));
        wait(timer, HOST + 0xb0, 1, 1, Error::PllTimeout)?;
        wait(timer, HOST + 0xb0, 0x94, 0x94, Error::LanesTimeout)?;
        write(HOST + 0x34, 1); // Command mode.
        write(HOST + 0x94, 3); // Automatic clock lane, HS request enabled.
        write(HOST + 0x98, (46 << 16) | 128);
        write(HOST + 0x9c, (50 << 16) | 104);
        write(HOST + 0x2c, 0x1d); // EoTp TX, BTA, ECC + CRC RX.
        write(HOST + 0x08, (7 << 8) | 4); // TO ~9.82MHz, escape ~17.19MHz.
        for offset in [0x78, 0x7c, 0x80, 0x84, 0x88, 0x8c] {
            write(HOST + offset, 0);
        }
        write(HOST + 0xf4, 6000);
        write(HOST + 0xa4, (0x3f << 8) | 1);
        // All generic/DCS packets in LP; request command ACK, no TE.
        write(HOST + 0x68, 0x010f_7f02);
        write(HOST + 0x30, 0); // Receive VC0.
        Ok(dsi)
    }

    /// Snapshot diagnostic registers, including error latches.
    ///
    /// # Safety
    /// The DSI APB clock must be enabled (including after `new` fails).
    /// Caller must own DSI exclusively; reading error latches can consume them.
    pub unsafe fn status() -> Status {
        Status {
            video_enabled: read(HOST + 0x34) & 1 == 0 && read(HOST + 0x04) & 1 != 0,
            version: read(HOST),
            phy: read(HOST + 0xb0),
            fifo: read(HOST + 0x74),
            errors0: read(HOST + 0xbc),
            errors1: read(HOST + 0xc0),
            video: read(HOST + 0x38),
            clock: read(CLOCK + 0x3c),
            ref20: read(CLOCK + 0x28),
            ldo: read(LDO),
            ldo_analog: read(LDO + 4),
            color_coding: read(HOST + 0x10),
            hline: read(HOST + 0x50),
            lp_command_size: read(HOST + 0x18),
            clock_dividers: read(HOST + 0x08),
        }
    }

    /// Snapshot host flags, including those consumed by reply validation.
    /// Reading clears the accumulated error flags; these are bitsets, not counts.
    pub fn diagnostics(&mut self) -> Status {
        let mut status = unsafe { Self::status() };
        status.errors0 |= self.observed_errors[0];
        status.errors1 |= self.observed_errors[1];
        self.observed_errors = [0; 2];
        status
    }

    /// Start a continuously reloaded one-buffer RGB565 scanout on DW-GDMA CH2.
    /// # Safety
    /// Own DMA channel 1; PSRAM identity mapping covers `address..address+1228800`.
    /// Keep the framebuffer alive until stop_framebuffer. No global DMA resets.
    pub unsafe fn start_framebuffer(
        &mut self,
        address: usize,
        timer: &mut Timer,
    ) -> Result<(), Error> {
        self.stop_framebuffer();
        self.set_video_config(LaneRate::Mbps550, PixelFormat::Rgb565, timer)?;
        const DMA: usize = 0x5008_1000;
        const CH: usize = DMA + 0x200;
        modify(CLOCK + 0x14, 0, 1 << 13);
        modify(CLOCK + 0x18, 0, 1 << 5);
        write(DMA + 0x10, 1);
        write(HOST + 4, 0);
        write(BRIDGE + 4, 0);
        write(BRIDGE, 1);
        write(BRIDGE + 0x40, 1024 << 4);
        write(BRIDGE + 0x44, 1);
        modify(CLOCK + 0x28, 0, 1 << 30);
        modify(
            CLOCK + 0x3c,
            (3 << 5) | (0xff << 8),
            (1 << 5) | (4 << 8) | (1 << 7),
        );
        // Diagnostic: match the known-good VPG/SDK 48-MHz DPI clock.
        write(HOST + 0x48, (20 * 550 + 192) / 384);
        write(HOST + 0x4c, (136 * 550 + 192) / 384);
        write(HOST + 0x50, (1340 * 550 + 192) / 384);
        write(HOST + 0x38, 0xbf02); // VPG disabled, burst/LP retained.
        write(BRIDGE + 0x30, (600 << 16) | 634);
        write(BRIDGE + 0x34, (2 << 16) | 12);
        write(BRIDGE + 0x38, (1024 << 16) | 1340);
        write(BRIDGE + 0x3c, (20 << 16) | 136);
        write(BRIDGE + 0x18, 2); // RGB565.
        write(BRIDGE + 0x0c, (1 << 31) | 153600);
        write(BRIDGE + 0x88, 1 << 4); // DMA flow, one block per frame.
        modify(BRIDGE + 0x6c, 1 << 28, 0);
        write(BRIDGE + 8, 256);
        write(BRIDGE + 0x8c, 768);
        write(BRIDGE + 0x50, 0);
        write(BRIDGE + 0x54, 1);
        write(CH, address as u32);
        write(CH + 4, 0);
        write(CH + 8, 0x5010_5000);
        write(CH + 12, 0);
        write(CH + 0x10, 153599);
        write(
            CH + 0x18,
            (1 << 30) | 1 | (1 << 6) | (3 << 8) | (3 << 11) | (8 << 14) | (7 << 18),
        );
        write(CH + 0x1c, (1 << 6) | (15 << 7) | (1 << 15) | (15 << 16));
        write(CH + 0x20, 5); // Auto-reload source and destination each block.
        write(CH + 0x24, 1 | (5 << 17) | (7 << 23) | (3 << 27));
        write(CH + 0x80, u32::MAX);
        write(CH + 0x90, 0);
        write(CH + 0x98, u32::MAX);
        write(BRIDGE + 4, 1);
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
        write(DMA + 0x18, 0x202);
        write(HOST + 0x34, 0);
        write(HOST + 4, 1);
        write(BRIDGE + 0x40, (1024 << 4) | 1);
        write(BRIDGE + 0x44, 1);
        self.scanout = true;
        Ok(())
    }

    pub fn framebuffer_status(&mut self) -> [u32; 12] {
        let status = [
            read(0x5008_1018),
            read(0x5008_1288),
            read(0x5008_1230),
            read(0x5008_1200),
            read(BRIDGE + 0x58),
            read(BRIDGE + 0x14),
            read(BRIDGE + 0x40),
            read(BRIDGE + 0x88),
            read(BRIDGE + 0x18),
            read(HOST + 0x10),
            read(BRIDGE + 0x90),
            read(BRIDGE + 0x44),
        ];
        write(BRIDGE + 0x54, 1);
        status
    }

    pub fn stop_framebuffer(&mut self) {
        if !self.scanout {
            return;
        }
        self.scanout = false;
        write(BRIDGE + 0x40, 1024 << 4);
        write(BRIDGE + 0x44, 1);
        write(BRIDGE + 4, 0);
        write(0x5008_101c, 0x202); // Abort only channel 1.
        for _ in 0..20000 {
            if read(0x5008_1018) & 2 == 0 {
                return;
            }
        }
        panic!("DSI DMA abort timeout");
    }

    fn phy_write(&mut self, address: u8, value: u8) {
        write(HOST + 0xb4, 0);
        write(HOST + 0xb8, (1 << 16) | u32::from(address));
        write(HOST + 0xb4, 2);
        write(HOST + 0xb4, 0);
        write(HOST + 0xb8, u32::from(value));
        write(HOST + 0xb4, 2);
        write(HOST + 0xb4, 0);
    }

    /// VC0 DCS write: up to 63 parameters in command mode, 9 in live video
    /// (16-byte LP slot, including long-packet header/CRC). No split commands.
    pub fn write_dcs(
        &mut self,
        command: u8,
        params: &[u8],
        timer: &mut Timer,
    ) -> Result<(), Error> {
        let video = read(HOST + 0x34) & 1 == 0;
        if video && params.len() > 9 {
            return Err(Error::PacketTooLong);
        }
        let result = self.write_dcs_inner(command, params, timer);
        // FIFO empty means the packet was consumed by the packetizer, not that
        // its LP transmission/ACK has completed. Do not reset the host yet.
        if video {
            timer.delay_us(1_000);
        }
        result
    }

    fn write_dcs_inner(
        &mut self,
        command: u8,
        params: &[u8],
        timer: &mut Timer,
    ) -> Result<(), Error> {
        if params.len() > 63 {
            return Err(Error::PacketTooLong);
        }
        let size = params.len() + 1;
        let header = if size > 2 {
            let mut payload = [0u8; 64];
            payload[0] = command;
            payload[1..size].copy_from_slice(params);
            for bytes in payload[..size].chunks(4) {
                let mut word = [0; 4];
                word[..bytes.len()].copy_from_slice(bytes);
                wait(timer, HOST + 0x74, 8, 0, Error::FifoTimeout)?;
                write(HOST + 0x70, u32::from_le_bytes(word));
            }
            0x39 | ((size as u32) << 8)
        } else if size == 2 {
            0x15 | (u32::from(command) << 8) | (u32::from(params[0]) << 16)
        } else {
            0x05 | (u32::from(command) << 8)
        };
        self.header(header, timer)?;
        wait(timer, HOST + 0x74, 5, 5, Error::FifoTimeout)
    }

    fn header(&mut self, value: u32, timer: &mut Timer) -> Result<(), Error> {
        wait(timer, HOST + 0x74, 2, 0, Error::FifoTimeout)?;
        write(HOST + 0x6c, value);
        Ok(())
    }

    /// Read 1..4 bytes, using LP/BTA slots in vertical blanking during video.
    pub fn read_dcs(&mut self, command: u8, len: u8, timer: &mut Timer) -> Result<u32, Error> {
        let mut result = self.read_dcs_inner(command, len, timer);
        let host0 = read(HOST + 0xbc);
        let host1 = read(HOST + 0xc0);
        self.observed_errors[0] |= host0;
        self.observed_errors[1] |= host1;
        // Corrected single-bit ECC is reported in diagnostics but may be used.
        // Reject uncorrectable ECC, CRC, size, EoTp and receive errors.
        if host0 != 0 || host1 & 0x1078 != 0 {
            result = Err(Error::ReadResponse { host0, host1 });
        }
        // Reset a stuck command/RX state on failure, without resetting PHY.
        if result.is_err() {
            write(HOST + 0x04, 0);
            timer.delay_us(10);
            write(HOST + 0x04, 1);
            // A malformed reply can leave delayed PHY/RX error latches after
            // reset. Allow a full video frame/BTA to settle, then consume them.
            timer.delay_us(20_000);
            self.observed_errors[0] |= read(HOST + 0xbc);
            self.observed_errors[1] |= read(HOST + 0xc0);
            for _ in 0..16 {
                if read(HOST + 0x74) & (1 << 4) != 0 {
                    break;
                }
                let _ = read(HOST + 0x70);
            }
        }
        result
    }

    fn read_dcs_inner(&mut self, command: u8, len: u8, timer: &mut Timer) -> Result<u32, Error> {
        if !(1..=4).contains(&len) {
            return Err(Error::PacketTooLong);
        }
        wait(timer, HOST + 0x74, 1 << 6, 0, Error::ReadTimeout)?;
        // Discard stale RX from a prior interrupted transaction, bounded.
        for _ in 0..16 {
            if read(HOST + 0x74) & (1 << 4) != 0 {
                break;
            }
            let _ = read(HOST + 0x70);
        }
        if read(HOST + 0x74) & (1 << 4) == 0 {
            return Err(Error::ReceiveFifo);
        }
        self.header(0x37 | (u32::from(len) << 8), timer)?;
        wait(timer, HOST + 0x74, 5, 5, Error::FifoTimeout)?;
        self.header(0x06 | (u32::from(command) << 8), timer)?;
        wait(timer, HOST + 0x74, 1 << 4, 0, Error::ReadTimeout)?;
        let value = read(HOST + 0x70);
        wait(timer, HOST + 0x74, 1 << 6, 0, Error::ReadTimeout)?;
        if read(HOST + 0x74) & (1 << 4) == 0 {
            return Err(Error::ReceiveFifo);
        }
        Ok(if len == 4 {
            value
        } else {
            value & ((1 << (len * 8)) - 1)
        })
    }

    /// Stop/restart video without changing its timing/pattern configuration.
    pub fn set_video_enabled(&mut self, enabled: bool) {
        write(HOST + 0x04, 0);
        write(HOST + 0x34, u32::from(!enabled));
        write(HOST + 0x04, 1);
    }

    /// Retune the diagnostic video path. Restores the prior link if PLL setup fails.
    /// Blanks video briefly; this does not configure a DMA/bridge framebuffer.
    pub fn set_video_config(
        &mut self,
        rate: LaneRate,
        format: PixelFormat,
        timer: &mut Timer,
    ) -> Result<(), Error> {
        let power = read(HOST + 0x04);
        write(HOST + 0x04, 0);
        if rate != self.rate {
            if let Err(error) = self.program_rate(rate, timer) {
                if self.program_rate(self.rate, timer).is_err() {
                    return Err(Error::LinkRecovery);
                }
                write(HOST + 0x04, power);
                return Err(error);
            }
        }
        self.rate = rate;
        self.format = format;
        self.video_timing();
        write(HOST + 0x04, power);
        Ok(())
    }

    fn program_rate(&mut self, rate: LaneRate, timer: &mut Timer) -> Result<(), Error> {
        write(HOST + 0xa0, 0x0d); // Assert digital PHY reset, keep ref/clock powered.
        let range = match rate {
            LaneRate::Mbps550 => 0x17,
            LaneRate::Mbps650 => 0x18,
            LaneRate::Mbps750 => 0x19,
        };
        let m = rate.mbps() / 5; // f_ref=20 MHz, N=4, all supported M are even.
        self.phy_write(0x44, range << 1);
        self.phy_write(0x19, 0x30);
        self.phy_write(0x17, 3);
        self.phy_write(0x18, ((m - 1) & 0x1f) as u8);
        self.phy_write(0x18, (0x80 | ((m - 1) >> 5)) as u8);
        write(HOST + 0xa0, 0x0f);
        wait(timer, HOST + 0xb0, 1, 1, Error::PllTimeout)?;
        wait(timer, HOST + 0xb0, 0x94, 0x94, Error::LanesTimeout)?;
        let timeout_div = (rate.mbps() + 40) / 80;
        let escape_div = (rate.mbps() + 72) / 144;
        write(HOST + 0x08, (timeout_div << 8) | escape_div);
        Ok(())
    }

    fn video_timing(&mut self) {
        write(
            HOST + 0x10,
            match self.format {
                PixelFormat::Rgb565 => 0,
                PixelFormat::Rgb888 => 5,
            },
        );
        // Host periods in byte-clock ticks, expected pixel clock = 50 MHz.
        let rate = self.rate.mbps();
        write(HOST + 0x48, (20 * rate + 200) / 400);
        write(HOST + 0x4c, (136 * rate + 200) / 400);
        write(HOST + 0x50, (1340 * rate + 200) / 400);
    }

    /// 1024x600 test pattern, using the selected format/rate and board timings.
    /// Uses the host's VPG, so no framebuffer or DMA is needed.
    pub fn start_pattern(&mut self, pattern: Pattern) {
        self.stop_framebuffer();
        write(HOST + 0x04, 0);
        // Keep the external DPI input disabled. VPG supplies pixels internally.
        write(BRIDGE + 0x40, 0);
        write(BRIDGE + 0x44, 1);
        // SPLL/2/5 = 48 MHz DPI (SDK rounds 240/50 to a divider of 5).
        modify(CLOCK + 0x28, 0, 1 << 30);
        modify(
            CLOCK + 0x3c,
            (3 << 5) | (0xff << 8),
            (1 << 5) | (4 << 8) | (1 << 7),
        );
        write(HOST + 0x0c, 0); // VC0.
        self.video_timing();
        write(HOST + 0x14, 0); // Timing polarity: all active high.
                               // At most 16 LP command bytes per blanking line; none during VACT.
                               // Without this, LP_CMD_EN alone cannot schedule runtime commands.
        write(HOST + 0x18, 16 << 16);
        write(HOST + 0x3c, 1024);
        write(HOST + 0x40, 0);
        write(HOST + 0x44, 0);
        write(HOST + 0x54, 2);
        write(HOST + 0x58, 12);
        write(HOST + 0x5c, 20);
        write(HOST + 0x60, 600);
        self.set_pattern(pattern);
        write(HOST + 0x34, 0); // Video mode.
        write(HOST + 0x04, 1);
    }

    pub fn set_pattern(&mut self, pattern: Pattern) {
        // Change VPG configuration while the host is in standby, so a frame
        // cannot mix two orientations. PHY PLL and panel state are retained.
        let power = read(HOST + 0x04);
        write(HOST + 0x04, 0);
        let mode = match pattern {
            Pattern::Vertical => 0,
            Pattern::Horizontal => 1 << 24,
            Pattern::Ber => 1 << 20,
        };
        // Burst with sync pulses, LP in blanking, no per-frame BTA.
        write(HOST + 0x38, mode | (1 << 16) | 0xbf02);
        write(HOST + 0x04, power);
    }
}

fn wait(
    timer: &mut Timer,
    address: usize,
    mask: u32,
    value: u32,
    error: Error,
) -> Result<(), Error> {
    let start = timer.now_us();
    while read(address) & mask != value {
        if timer.now_us().wrapping_sub(start) & ((1 << 48) - 1) >= 100_000 {
            return Err(error);
        }
    }
    Ok(())
}

fn read(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}
fn write(address: usize, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) };
}
fn modify(address: usize, clear: u32, set: u32) {
    write(address, (read(address) & !clear) | set);
}

fn enable_ldo3() {
    let version = read(0x5012_d04c);
    let calibration = if version & (0x1f << 8) != 0 {
        read(0x5012_d050)
    } else {
        0
    };
    let k = (calibration >> 6) & 0xff;
    let vos = (calibration >> 14) & 0x3f;
    let c = (calibration >> 20) & 0x3f;
    let k = if k == 0 {
        1000
    } else {
        signed_magnitude(k, 0x80) + 975
    };
    let vos = if vos == 0 {
        0
    } else {
        signed_magnitude(vos, 0x20) - 3
    };
    let c = if c == 0 {
        1000
    } else {
        signed_magnitude(c, 0x20) + 990
    };
    let mut best = (i32::MAX, 0, 0);
    for dref in 0..16 {
        let vref20 = if dref < 9 {
            10 + dref
        } else {
            20 + (dref - 9) * 2
        };
        for mul in 0..8 {
            let voltage = (vref20 * k + 20 * vos) * (4000 + mul * c);
            let diff = (2500 * 80000 - voltage).abs();
            if diff < best.0 {
                best = (diff, dref, mul);
            }
        }
    }
    modify(
        LDO + 4,
        0xf << 28 | 7 << 23,
        (best.1 as u32) << 28 | (best.2 as u32) << 23 | 1 << 27,
    );
    // Software ownership; bypass off; turn VO3 on and enable ripple suppression.
    modify(LDO, (7 << 9) | (1 << 14), (1 << 7) | (1 << 8));
    modify(LDO + 4, 1 << 27, 1 << 26);
}

fn signed_magnitude(value: u32, sign: u32) -> i32 {
    let magnitude = (value & (sign - 1)) as i32;
    if value & sign != 0 {
        -magnitude
    } else {
        magnitude
    }
}
