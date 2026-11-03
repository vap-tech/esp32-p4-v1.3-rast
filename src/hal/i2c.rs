//! I2C1 master, GPIO7 SDA / GPIO8 SCL, XTAL 40 MHz, standard mode 100 kHz.
//! Polling, one FIFO per direction, 7-bit addressing, repeated START.
use super::time::Timer;
use core::marker::PhantomData;

const BASE: usize = 0x500c_5000;
const GPIO: usize = 0x500e_0000;
const CLOCK: usize = 0x500e_6000;
const MASTER: u32 = 1 << 4;
const NACK: u32 = 1 << 10;
const TIMEOUT: u32 = (1 << 8) | (1 << 13);
const ARBITRATION: u32 = 1 << 5;
const COMPLETE: u32 = 1 << 7;
const FIFO_CONFIG: u32 = (1 << 14) | 31 | (4 << 5);
const US_MASK: u64 = (1 << 48) - 1;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidRequest,
    BusBusy,
    Nack,
    Timeout,
    Arbitration,
    Fifo,
}

pub struct I2c {
    _exclusive: PhantomData<*mut ()>,
}

// All callers use fixed registers for this controller or its pin/clock setup.
unsafe fn read(addr: usize) -> u32 {
    unsafe { (addr as *const u32).read_volatile() }
}
unsafe fn write(addr: usize, value: u32) {
    unsafe { (addr as *mut u32).write_volatile(value) }
}
unsafe fn modify(addr: usize, clear: u32, set: u32) {
    unsafe { write(addr, (read(addr) & !clear) | set) };
}

impl I2c {
    /// # Safety
    /// Own I2C1, GPIO7/8 and their matrix/clock controls exclusively. Single-core
    /// setup, interrupts disabled; the board crystal must be 40 MHz.
    pub unsafe fn new(timer: &mut Timer) -> Self {
        unsafe {
            modify(CLOCK + 0x1c, 0, 1 << 13); // I2C1 APB.
            modify(CLOCK + 0x40, 1 << 26, 1 << 27); // XTAL, functional clock.
            modify(CLOCK + 0x44, 0x00ff_ffff, 0); // Divider 1, no fraction.
            modify(CLOCK + 0xc4, 0, 1 << 21);
            modify(CLOCK + 0xc4, 1 << 21, 0);
            modify(CLOCK + 0x20, 0, 1 << 3); // IO_MUX APB.
            modify(CLOCK + 0xac, 0, 1 << 9); // IO_MUX functional clock.
            for (pin, signal) in [(7usize, 71usize), (8, 70)] {
                write(GPIO + 0x28, 1 << pin); // Disable pad while rerouting.
                write(GPIO + 8, 1 << pin); // Release open-drain latch.
                modify(GPIO + 0x74 + 4 * pin, 0, 1 << 2); // Pad open-drain.
                write(GPIO + 0x558 + 4 * pin, signal as u32 | (1 << 10));
                write(GPIO + 0x158 + 4 * signal, pin as u32 | (1 << 7));
                // GPIO function, input, pull-up; no pull-down or sleep mux.
                modify(
                    0x500e_1004 + 4 * pin,
                    (7 << 12) | (1 << 7) | (1 << 1),
                    (1 << 12) | (1 << 9) | (1 << 8),
                );
                write(GPIO + 0x24, 1 << pin);
            }
        }
        let mut bus = Self {
            _exclusive: PhantomData,
        };
        bus.configure(timer);
        bus
    }

    fn configure(&mut self, timer: &mut Timer) {
        unsafe {
            write(BASE + 4, MASTER | (1 << 10)); // Reset master FSM.
            timer.delay_us(5);
            write(BASE + 4, MASTER); // OD, MSB first; no arbitration/multi-master.
            write(BASE, 199); // 200 XTAL ticks low.
            write(BASE + 0x38, 102 | (98 << 9)); // High plus wait-high = 200 ticks.
            write(BASE + 0x30, 49); // SDA hold: 50 ticks.
            write(BASE + 0x34, 99); // SDA sample: 100 ticks.
            for offset in [0x40, 0x44, 0x48, 0x4c] {
                write(BASE + offset, 199);
            }
            write(BASE + 0x0c, 17 | (1 << 5)); // SCL timeout 2^17 / 40 MHz.
            write(BASE + 0x50, 7 | (7 << 4) | (3 << 8)); // Glitch filters.
            write(BASE + 0x18, FIFO_CONFIG | (1 << 12) | (1 << 13));
            write(BASE + 0x18, FIFO_CONFIG); // FIFO mode, reset released.
            write(BASE + 0x28, 0); // Poll raw flags; no CPU interrupts.
            write(BASE + 0x24, u32::MAX);
            write(BASE + 4, MASTER | (1 << 11)); // Synchronize configuration.
        }
        timer.delay_us(5);
    }

    /// Address-only write transaction (START, address, ACK check, STOP).
    pub fn probe(&mut self, address: u8, timer: &mut Timer) -> Result<(), Error> {
        self.write_read(address, &[], &mut [], timer)
    }

    /// Write up to 30 bytes and read up to 32 bytes in one bounded transaction.
    /// Uses repeated START between nonempty write/read phases; the last read
    /// byte is NACKed. Empty phases support write-only, read-only and probing.
    pub fn write_read(
        &mut self,
        address: u8,
        tx: &[u8],
        rx: &mut [u8],
        timer: &mut Timer,
    ) -> Result<(), Error> {
        if !(0x08..=0x77).contains(&address) || tx.len() > 30 || rx.len() > 32 {
            return Err(Error::InvalidRequest);
        }
        let start = timer.now_us();
        unsafe {
            while read(BASE + 8) & (1 << 4) != 0 {
                if timer.now_us().wrapping_sub(start) & US_MASK >= 2_000 {
                    self.configure(timer);
                    return Err(Error::BusBusy);
                }
            }
            write(BASE + 0x18, FIFO_CONFIG | (1 << 12) | (1 << 13));
            write(BASE + 0x18, FIFO_CONFIG);
            write(BASE + 0x24, u32::MAX);
            for i in 0..8 {
                write(BASE + 0x58 + 4 * i, 4 << 11);
            } // END.
            let mut command = 0usize;
            let mut cmd = |value| {
                write(BASE + 0x58 + 4 * command, value);
                command += 1;
            };
            cmd(6 << 11); // START.
            if !tx.is_empty() || rx.is_empty() {
                write(BASE + 0x1c, u32::from(address) << 1);
                for &byte in tx {
                    write(BASE + 0x1c, u32::from(byte));
                }
                cmd((1 << 11) | (1 << 8) | (tx.len() as u32 + 1)); // WRITE + ACK check.
            }
            if !rx.is_empty() {
                if !tx.is_empty() {
                    cmd(6 << 11);
                } // Repeated START.
                write(BASE + 0x1c, (u32::from(address) << 1) | 1);
                cmd((1 << 11) | (1 << 8) | 1); // Read address, ACK check.
                if rx.len() > 1 {
                    cmd((3 << 11) | (rx.len() as u32 - 1));
                }
                cmd((3 << 11) | (1 << 10) | 1); // Last byte, send NACK.
            }
            cmd(2 << 11); // STOP.
            write(BASE + 4, MASTER | (1 << 11));
            modify(BASE + 4, 0, 1 << 5); // TRANS_START.
            let start = timer.now_us();
            let mut received = 0;
            loop {
                let flags = read(BASE + 0x20);
                let error = if flags & NACK != 0 {
                    Some(Error::Nack)
                } else if flags & ARBITRATION != 0 {
                    Some(Error::Arbitration)
                } else if flags & ((1 << 2) | (1 << 11)) != 0 {
                    Some(Error::Fifo)
                } else if flags & TIMEOUT != 0
                    || timer.now_us().wrapping_sub(start) & US_MASK >= 20_000
                {
                    Some(Error::Timeout)
                } else {
                    None
                };
                if let Some(error) = error {
                    self.configure(timer);
                    return Err(error);
                }
                // Drain while hardware runs: a full RX FIFO can stall the final
                // NACK/STOP. No ISR or DMA is needed at standard-mode speed.
                let available = ((read(BASE + 8) >> 8) & 0x3f) as usize;
                if received + available > rx.len() {
                    self.configure(timer);
                    return Err(Error::Fifo);
                }
                for byte in &mut rx[received..received + available] {
                    *byte = read(BASE + 0x1c) as u8;
                }
                received += available;
                if flags & COMPLETE != 0 {
                    break;
                }
            }
            if received != rx.len() {
                self.configure(timer);
                return Err(Error::Fifo);
            }
            write(BASE + 0x24, u32::MAX);
        }
        Ok(())
    }
}
