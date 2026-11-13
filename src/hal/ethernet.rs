// Clock/register sequences adapted from ESP-IDF v6.1 esp_hal_emac.
// SPDX-FileCopyrightText: 2023-2026 Espressif Systems (Shanghai) CO LTD
// SPDX-License-Identifier: Apache-2.0
//! P4 rev1.x EMAC, board SMI on MDC31 / MDIO52. No IRQ or SDK runtime.
//! Register definitions checked against local ESP-IDF v6.1 esp_hal_emac/hw_ver1.
use super::time::Timer;

const MAC: usize = 0x5009_8000;
const GPIO: usize = 0x500e_0000;
const IOMUX: usize = 0x500e_1000;
const CLOCK: usize = 0x500e_6000;

#[derive(Clone, Copy, Debug)]
pub enum Error {
    MdioTimeout,
    Address,
    CacheEnabled,
    Memory,
    ResetTimeout,
    TransmitTimeout,
    Transmit(u32),
    Dma(u32),
    Frame(u32),
}

pub struct Mdio {
    _exclusive: core::marker::PhantomData<*mut ()>,
}
impl Mdio {
    /// # Safety
    /// Own EMAC and GPIO31/52 exclusively, PHY already powered by board hardware.
    /// Does not reset PHY or change its clock, straps, autonegotiation or registers.
    pub unsafe fn new() -> Self {
        modify(CLOCK + 0x18, 0, 1 << 13);
        modify(CLOCK + 0x20, 0, 1 << 3);
        modify(CLOCK + 0xac, 0, 1 << 9);
        for pin in [31, 52] {
            modify(
                IOMUX + 4 + pin * 4,
                (7 << 12) | (1 << 7) | (1 << 8),
                (1 << 12) | (1 << 9),
            );
            modify(GPIO + 0x74 + pin * 4, 1 << 2, 0);
        }
        write(GPIO + 0x558 + 31 * 4, 108 | (1 << 10));
        write(GPIO + 0x24, 1 << 31);
        write(GPIO + 0x558 + 52 * 4, 109);
        write(GPIO + 0x30, 1 << (52 - 32));
        write(GPIO + 0x158 + 107 * 4, 52 | (1 << 7));
        Self {
            _exclusive: core::marker::PhantomData,
        }
    }
    pub fn version(&self) -> u32 {
        read(MAC + 0x20)
    }
    pub fn read(&mut self, phy: u8, reg: u8, timer: &mut Timer) -> Result<u16, Error> {
        if phy >= 32 || reg >= 32 {
            return Err(Error::Address);
        }
        wait_clear(MAC + 0x10, 1, timer, 10_000)?;
        // Conservative CSR / 102 (safe up to 250 MHz); Clause 22 read.
        write(
            MAC + 0x10,
            (u32::from(phy) << 11) | (u32::from(reg) << 6) | (4 << 2) | 1,
        );
        wait_clear(MAC + 0x10, 1, timer, 10_000)?;
        Ok(read(MAC + 0x14) as u16)
    }
}
fn wait_clear(addr: usize, mask: u32, timer: &mut Timer, timeout: u64) -> Result<(), Error> {
    let start = timer.now_us();
    while read(addr) & mask != 0 {
        if timer.now_us().wrapping_sub(start) >= timeout {
            return Err(Error::MdioTimeout);
        }
    }
    Ok(())
}
fn read(addr: usize) -> u32 {
    unsafe { (addr as *const u32).read_volatile() }
}
fn write(addr: usize, value: u32) {
    unsafe { (addr as *mut u32).write_volatile(value) }
}
fn modify(addr: usize, clear: u32, set: u32) {
    write(addr, (read(addr) & !clear) | set);
}

const RX_COUNT: usize = 8;
const TX_COUNT: usize = 4;
const FRAME: usize = 1536;
const DESCS: usize = (RX_COUNT + TX_COUNT) * 64;
const BYTES: usize = DESCS + (RX_COUNT + TX_COUNT) * FRAME;
const DMA: usize = MAC + 0x1000;

/// Owns arena storage until DMA is synchronously stopped, including on errors.
/// Initial bring-up requires D-cache bypass and L2=0; cached DMA is not implied.
// Single EMAC owner; atomic counters keep diagnostics outside driver state.
static PROFILE: [core::sync::atomic::AtomicU32; 6] =
    [const { core::sync::atomic::AtomicU32::new(0) }; 6];
fn profile(index: usize, value: u64) {
    PROFILE[index].fetch_add(value as u32, core::sync::atomic::Ordering::Relaxed);
}
pub struct Emac<'a, 't> {
    storage: crate::memory::Buffer<'a, crate::memory::Internal>,
    timer: &'t mut Timer,
    base: usize,
    rx: usize,
    tx: usize,
    tx_pending: u8,
    active: bool,
}
impl<'a, 't> Emac<'a, 't> {
    /// # Safety
    /// Own EMAC, MDIO31/52 and RMII28/29/30/34/35/49/50 exclusively.
    /// GPIO50 must receive the PHY's 50-MHz reference clock. PHY link must be
    /// negotiated 100M full duplex. No other core may change caches or clocks.
    pub unsafe fn start(
        arena: &'a crate::memory::Arena<'_, crate::memory::Internal>,
        timer: &'t mut Timer,
        mac: [u8; 6],
    ) -> Result<Self, Error> {
        let cache = super::memory::cache_snapshot();
        if cache.l1_bypass & 16 == 0 || cache.l2_size != 0 {
            return Err(Error::CacheEnabled);
        }
        let _mdio = unsafe { Mdio::new() };
        for pin in [28, 29, 30, 34, 35, 49, 50] {
            modify(
                IOMUX + 4 + pin * 4,
                (7 << 12) | (1 << 7) | (1 << 8),
                (3 << 12) | (1 << 9),
            );
        }
        // RMII external clock, rx/tx use the common TXRX pad; preserve other clocks.
        modify(0x500e_514c, 7 << 2, 4 << 2);
        modify(CLOCK + 0x30, (7 << 24) | (1 << 28), (1 << 27) | (1 << 29));
        modify(CLOCK + 0x34, 0x7ffff, 1 | (1 << 9) | (1 << 10));
        modify(0x5011_1040, (1 << 13) | (1 << 14), 1 << 15);
        modify(CLOCK + 0xb4, 0, 1 << 17);
        write(DMA, 1);
        wait_clear(DMA, 1, timer, 100_000).map_err(|_| Error::ResetTimeout)?;
        let mut storage = arena
            .allocate(core::alloc::Layout::from_size_align(BYTES + 128, 64).unwrap())
            .map_err(|_| Error::Memory)?;
        storage.as_mut_slice()[..64].fill(0xa5);
        storage.as_mut_slice()[64 + BYTES..].fill(0xa5);
        let base = storage.as_mut_slice().as_mut_ptr() as usize + 64;
        let mut emac = Self {
            storage,
            timer,
            base,
            rx: 0,
            tx: 0,
            tx_pending: 0,
            active: false,
        };
        for i in 0..RX_COUNT {
            let desc = base + i * 64;
            write(desc + 4, FRAME as u32 | (1 << 14) | (1 << 31));
            write(desc + 8, (base + DESCS + i * FRAME) as u32);
            write(desc + 12, (base + ((i + 1) % RX_COUNT) * 64) as u32);
            write(desc, 1 << 31);
        }
        let tx = base + RX_COUNT * 64;
        for i in 0..TX_COUNT {
            let desc = tx + i * 64;
            write(desc + 8, (base + DESCS + (RX_COUNT + i) * FRAME) as u32);
            write(desc + 12, (tx + ((i + 1) % TX_COUNT) * 64) as u32);
            write(desc, 1 << 20);
        }
        write(MAC + 0x40, u16::from_le_bytes([mac[4], mac[5]]) as u32);
        write(MAC + 0x44, u32::from_le_bytes(mac[..4].try_into().unwrap()));
        write(MAC + 4, 0); // Own unicast + broadcasts, no promiscuous mode.
        write(MAC + 0x3c, u32::MAX); // Mask MAC interrupts.
        write(DMA + 0x1c, 0); // Polling only.
        write(DMA + 0x14, u32::MAX);
        write(DMA, (1 << 7) | (16 << 8) | (1 << 25) | (1 << 26));
        write(DMA + 0xc, base as u32);
        write(DMA + 0x10, tx as u32);
        // P4 FIFO is only 256 bytes: no receive/transmit store-and-forward.
        // Flush TX, thresholds default 64 B, operate on second frame.
        write(DMA + 0x18, 1 << 20);
        wait_clear(DMA + 0x18, 1 << 20, emac.timer, 10_000).map_err(|_| Error::ResetTimeout)?;
        fence();
        write(MAC, (1 << 15) | (1 << 14) | (1 << 11) | (1 << 2) | (1 << 3));
        write(DMA + 0x18, (1 << 13) | (1 << 2) | (1 << 1));
        emac.active = true;
        Ok(emac)
    }
    /// Temporarily lend the sole timer to another polled device. No EMAC method
    /// can execute through this borrow while the operation is running.
    pub fn with_timer<R>(&mut self, operation: impl FnOnce(&mut Timer) -> R) -> R {
        operation(self.timer)
    }
    pub fn now_us(&mut self) -> u64 {
        self.timer.now_us()
    }
    pub fn stats(&self) -> [u32; 6] {
        core::array::from_fn(|i| PROFILE[i].load(core::sync::atomic::Ordering::Relaxed))
    }
    pub fn reset_stats(&mut self) {
        for counter in &PROFILE {
            counter.store(0, core::sync::atomic::Ordering::Relaxed);
        }
    }
    /// Read-to-clear RX buffer/FIFO loss counters, diagnostics owns sampling.
    #[cfg(feature = "diagnostics")]
    pub fn take_rx_losses(&mut self) -> u32 {
        read(DMA + 0x20)
    }
    pub fn status(&self) -> [u32; 6] {
        [
            read(DMA + 0x14),
            read(DMA + 0x20),
            read(DMA + 0x4c),
            read(DMA + 0x54),
            read(MAC),
            self.storage.len() as u32,
        ]
    }
    pub fn guards_ok(&self) -> bool {
        (0..64).all(|i| unsafe {
            ((self.base - 64 + i) as *const u8).read_volatile() == 0xa5
                && ((self.base + BYTES + i) as *const u8).read_volatile() == 0xa5
        })
    }
    /// Copies a completed, single-descriptor frame, then returns OWN to DMA.
    /// Frame length includes trailing FCS if the MAC supplies it.
    pub fn receive(&mut self, out: &mut [u8; FRAME]) -> Result<Option<usize>, Error> {
        self.reap_tx()?;
        profile(0, 1);
        let dma_status = read(DMA + 0x14);
        if dma_status & (1 << 13) != 0 {
            return Err(Error::Dma(dma_status));
        }
        let desc = self.base + self.rx * 64;
        let header = read(desc);
        if header >> 31 != 0 {
            return Ok(None);
        }
        fence();
        let len = ((header >> 16) & 0x3fff) as usize;
        let valid =
            header & (1 << 15) == 0 && header & (3 << 8) == (3 << 8) && (14..=FRAME).contains(&len);
        if valid {
            profile(1, 1);
            let started = self.timer.now_us();
            copy_from_dma(self.base + DESCS + self.rx * FRAME, &mut out[..len]);
            profile(2, self.timer.now_us() - started);
        }
        fence();
        write(desc, 1 << 31);
        fence();
        self.rx = (self.rx + 1) % RX_COUNT;
        write(DMA + 8, 1);
        if valid {
            Ok(Some(len))
        } else {
            Err(Error::Frame(header))
        }
    }
    pub fn transmit(&mut self, frame: &[u8]) -> Result<(), Error> {
        if !(14..=1514).contains(&frame.len()) {
            return Err(Error::Address);
        }
        self.reap_tx()?;
        profile(3, 1);
        let desc = self.base + (RX_COUNT + self.tx) * 64;
        let started = self.timer.now_us();
        wait_clear(desc, 1 << 31, self.timer, 10_000).map_err(|_| Error::TransmitTimeout)?;
        profile(5, self.timer.now_us() - started);
        self.reap_tx()?;
        let started = self.timer.now_us();
        copy_to_dma(self.base + DESCS + (RX_COUNT + self.tx) * FRAME, frame);
        profile(4, self.timer.now_us() - started);
        write(desc + 4, frame.len() as u32);
        fence();
        write(desc, (1 << 31) | (1 << 29) | (1 << 28) | (1 << 20));
        fence();
        self.tx_pending |= 1 << self.tx;
        self.tx = (self.tx + 1) % TX_COUNT;
        write(DMA + 4, 1);
        Ok(())
    }
    // Reclaim only descriptors returned by DMA, checking every completion even
    // when no further TX is queued. Never inspect a DMA-owned status as final.
    fn reap_tx(&mut self) -> Result<(), Error> {
        let dma_status = read(DMA + 0x14);
        if dma_status & (1 << 13) != 0 {
            return Err(Error::Dma(dma_status));
        }
        for i in 0..TX_COUNT {
            if self.tx_pending & (1 << i) == 0 {
                continue;
            }
            let status = read(self.base + (RX_COUNT + i) * 64);
            if status & (1 << 31) != 0 {
                continue;
            }
            fence();
            self.tx_pending &= !(1 << i);
            if status & (1 << 15) != 0 {
                return Err(Error::Transmit(status));
            }
        }
        Ok(())
    }
    #[inline(never)]
    fn stop(&mut self) {
        if !self.active {
            return;
        }
        modify(DMA + 0x18, (1 << 13) | (1 << 1), 0);
        let started = self.timer.now_us();
        while read(DMA + 0x14) & ((7 << 17) | (7 << 20)) != 0 {
            if self.timer.now_us() - started > 10_000 {
                write(DMA, 1);
                wait_clear(DMA, 1, self.timer, 100_000)
                    .expect("EMAC DMA did not stop; keep storage reserved");
                break;
            }
        }
        modify(MAC, (1 << 2) | (1 << 3), 0);
        fence();
        self.active = false;
        assert!(self.guards_ok(), "EMAC DMA storage guard damaged");
    }
}
impl Drop for Emac<'_, '_> {
    fn drop(&mut self) {
        self.stop();
    }
}
// DMA buffers are word-aligned internal SRAM. CPU slices need no alignment.
// Tail bytes are handled separately: neither path reads/writes past the frame.
fn copy_from_dma(address: usize, out: &mut [u8]) {
    let (words, tail) = out.as_chunks_mut::<4>();
    for (i, word) in words.iter_mut().enumerate() {
        *word = read(address + i * 4).to_le_bytes();
    }
    let offset = words.len() * 4;
    for (i, byte) in tail.iter_mut().enumerate() {
        *byte = unsafe { ((address + offset + i) as *const u8).read_volatile() };
    }
}
fn copy_to_dma(address: usize, bytes: &[u8]) {
    let (words, tail) = bytes.as_chunks::<4>();
    for (i, word) in words.iter().enumerate() {
        write(address + i * 4, u32::from_le_bytes(*word));
    }
    let offset = words.len() * 4;
    for (i, byte) in tail.iter().enumerate() {
        unsafe { ((address + offset + i) as *mut u8).write_volatile(*byte) };
    }
}
fn fence() {
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn word_copy_handles_unaligned_cpu_slices_and_exact_tails() {
        for len in [1, 2, 3, 4, 5, 14, 63, 64, 65, 1500, 1514, 1536] {
            for shift in 0..4 {
                let mut input = [0u8; 1540];
                for (i, b) in input.iter_mut().enumerate() {
                    *b = i as u8;
                }
                let mut dma = [0xa5a5a5a5u32; 386];
                let address = dma.as_mut_ptr() as usize + 4;
                super::copy_to_dma(address, &input[shift..shift + len]);
                assert_eq!(dma[0], 0xa5a5a5a5);
                let raw = unsafe { core::slice::from_raw_parts(address as *const u8, 1540) };
                assert!(raw[len..].iter().all(|b| *b == 0xa5));
                let mut output = [0xabu8; 1540];
                super::copy_from_dma(address, &mut output[shift..shift + len]);
                assert_eq!(&output[shift..shift + len], &input[shift..shift + len]);
                assert!(output[..shift]
                    .iter()
                    .chain(output[shift + len..].iter())
                    .all(|b| *b == 0xab));
            }
        }
    }
}
