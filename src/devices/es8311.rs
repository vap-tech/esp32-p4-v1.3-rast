//! ES8311 DAC, 16kHz PCM16 Philips I2S, slave with 256fs MCLK.
//! Register sequence adapted from Espressif esp_codec_dev (Apache-2.0).
use crate::hal::{
    i2c::{Error, I2c},
    time::Timer,
};
const ADDRESS: u8 = 0x18;
pub struct Es8311;
impl Es8311 {
    pub fn read(bus: &mut I2c, timer: &mut Timer, register: u8) -> Result<u8, Error> {
        let mut value = [0];
        bus.write_read(ADDRESS, &[register], &mut value, timer)?;
        Ok(value[0])
    }
    fn write(bus: &mut I2c, timer: &mut Timer, register: u8, value: u8) -> Result<(), Error> {
        bus.write_read(ADDRESS, &[register, value], &mut [], timer)
    }
    pub fn new(bus: &mut I2c, timer: &mut Timer) -> Result<Self, Error> {
        if Self::read(bus, timer, 0xfd)? != 0x83 || Self::read(bus, timer, 0xfe)? != 0x11 {
            return Err(Error::InvalidRequest);
        }
        for (reg, value) in [
            (0x0d, 0xfa),
            (0x44, 0x08),
            (0x44, 0x08),
            (0x01, 0x30),
            (0x02, 0),
            (0x03, 0x10),
            (0x16, 0x24),
            (0x04, 0x20),
            (0x05, 0),
            (0x0b, 0),
            (0x0c, 0),
            (0x10, 0x1f),
            (0x11, 0x7f),
            (0x00, 0x80),
            (0x01, 0x3f),
            (0x06, 3),
            (0x07, 0),
            (0x08, 0xff),
            (0x09, 0x0c),
            (0x0a, 0x4c),
            (0x13, 0x10),
            (0x1b, 0x0a),
            (0x1c, 0x6a),
            (0x44, 0x58), // Match old codec_cfg.no_dac_ref=false (ADC + DACR serial reference).
            (0x17, 0xbf),
            (0x0e, 2),
            (0x12, 0),
            (0x14, 0x1a),
            (0x0d, 1),
            (0x15, 0x40),
            (0x37, 0x08),
            (0x45, 0),
            (0x31, 0x60),
            (0x32, 0x95), // -21 dB, 3 dB above the earlier quiet baseline
        ] {
            Self::write(bus, timer, reg, value)?;
        }
        timer.delay_us(50_000);
        Ok(Self)
    }
    /// Enable analog microphone SDOUT, left ADC + right DAC reference, 24dB PGA.
    /// PA remains off; caller provides the I2S clocks before initialization.
    pub fn microphone(bus: &mut I2c, timer: &mut Timer) -> Result<Self, Error> {
        let mut codec = Self::new(bus, timer)?;
        codec.mute(bus, timer, true)?;
        Self::write(bus, timer, 0x0a, 0x0c)?; // Enable ADC serial output, PCM16.
        Self::write(bus, timer, 0x14, 0x1a)?; // Analog mic, not DMIC.
        Self::write(bus, timer, 0x16, 4)?; // 24dB PGA (same as old recording firmware).
        Self::write(bus, timer, 0x44, 0x58)?; // Left microphone, right DAC reference.
        timer.delay_us(50_000);
        Ok(codec)
    }
    pub fn mute(&mut self, bus: &mut I2c, timer: &mut Timer, mute: bool) -> Result<(), Error> {
        let reg = Self::read(bus, timer, 0x31)? & !0x60;
        Self::write(bus, timer, 0x31, reg | if mute { 0x60 } else { 0 })
    }
    /// Route digital DACL/DACR to SDOUT for transport diagnosis (no microphone).
    pub fn reference_output(
        &mut self,
        bus: &mut I2c,
        timer: &mut Timer,
        enabled: bool,
    ) -> Result<(), Error> {
        Self::write(bus, timer, 0x44, if enabled { 0x68 } else { 0x58 })?;
        Self::write(bus, timer, 0x0a, if enabled { 0x0c } else { 0x4c })
    }
    pub fn snapshot(&self, bus: &mut I2c, timer: &mut Timer) -> Result<[u8; 12], Error> {
        let mut regs = [0; 12];
        for (i, reg) in [
            0x00, 0x01, 0x02, 0x04, 0x06, 0x08, 0x09, 0x12, 0x31, 0x32, 0x44, 0xff,
        ]
        .iter()
        .enumerate()
        {
            regs[i] = Self::read(bus, timer, *reg)?;
        }
        Ok(regs)
    }
}
