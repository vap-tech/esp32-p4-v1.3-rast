//! P4 rev1.x baseline JPEG + dedicated 2D-DMA channels0, polled bring-up.
//! Register/sequence reference: local ESP-IDF v6.1 esp_hal_jpeg/esp_hal_dma.
pub mod header;
pub use header::Header;
/// JPEG uses full-range YCbCr; video BT.601 is an explicit alternative.
#[derive(Debug, Clone, Copy)]
pub enum ColorConversion {
    JpegFull,
    Bt601Limited,
}
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub jpeg: u32,
    pub rx: u32,
    pub tx: u32,
    pub rx_state: u32,
    pub tx_state: u32,
    pub decoder: u32,
    pub axi: u32,
    pub axi_reset: u32,
    pub axi_ready: u32,
    pub axi_tx: u32,
    pub axi_rx: u32,
    pub axi_drained: u32,
    pub axi_aborted: u32,
}
#[derive(Debug)]
pub enum Error {
    Header(header::Error),
    Memory,
    Busy,
    Cache,
    Hardware(Snapshot),
    Timeout(Snapshot),
}
#[cfg(all(target_arch = "riscv32", feature = "jpeg-lab"))]
mod hardware {
    use super::*;
    use crate::hal::dma2d::{bus_address, Claim};
    use crate::hal::time::Timer;
    use core::arch::asm;
    const JPEG: usize = 0x50086000;
    const DMA: usize = 0x50088000;
    const RX: usize = DMA + 0x500;
    const CLK: usize = 0x500e6018;
    const RESET: usize = 0x500e60c0;
    fn r(a: usize) -> u32 {
        unsafe { (a as *const u32).read_volatile() }
    }
    fn w(a: usize, v: u32) {
        unsafe { (a as *mut u32).write_volatile(v) }
    }
    fn modify(a: usize, clear: u32, set: u32) {
        w(a, (r(a) & !clear) | set)
    }
    fn fence() {
        unsafe { asm!("fence iorw,iorw", options(nostack)) }
    }
    fn pulse(a: usize, bits: u32) {
        modify(a, 0, bits);
        fence();
        modify(a, bits, 0);
        fence();
    }
    fn snapshot() -> Snapshot {
        Snapshot {
            jpeg: r(JPEG + 0x38),
            rx: r(RX + 4),
            tx: r(DMA + 4),
            rx_state: r(RX + 0x24),
            tx_state: r(DMA + 0x24),
            decoder: r(JPEG + 0x88),
            axi: r(DMA + 0xa00),
            axi_reset: 0,
            axi_ready: 0,
            axi_tx: 0,
            axi_rx: 0,
            axi_drained: 0,
            axi_aborted: 0,
        }
    }
    fn physical(ptr: usize, len: usize) -> Result<u32, Error> {
        bus_address(ptr, len).ok_or(Error::Memory)
    }
    fn stop(timer: &mut Timer) -> u32 {
        // Stop the producer first. Abort waits for AXI/channel reset-safe conditions
        // before buffers/descriptors may be released, even following decode errors.
        modify(JPEG, 0, 1 << 24);
        modify(DMA, 0, 1 << 25);
        modify(RX, 0, 1 << 25);
        fence();
        let start = timer.now_us();
        while r(DMA + 0x24) & (1 << 24) == 0 || r(RX + 0x24) & (1 << 23) == 0 {
            assert!(
                timer.now_us() - start < 100_000,
                "JPEG DMA failed to quiesce; keep buffers reserved"
            );
        }
        let axi = r(DMA + 0xa00);
        pulse(DMA, 1 << 24);
        pulse(RX, 1 << 24);
        pulse(RESET, (1 << 30) | (1 << 31));
        fence();
        axi
    }
    fn configure(h: &Header) {
        // Module reset defaults enable the DQT/DHT FIFO ports. Width/height must
        // transition via zero before setting the next frame (IDF digital workaround).
        modify(JPEG, 0, 1 << 31);
        w(JPEG + 8, 0);
        w(JPEG + 4, 0x03020100);
        for table in 0..4 {
            if h.qmask & (1 << table) != 0 {
                for q in h.quant[table] {
                    w(JPEG + 0x10 + table * 4, u32::from(q));
                }
            }
        }
        w(JPEG + 8, u32::from(h.stride) << 16 | u32::from(h.rows));
        modify(
            JPEG + 0x20,
            0x00ff_ffff,
            u32::from(h.components) << 16 | u32::from(h.restart),
        );
        for i in 0..usize::from(h.components) {
            let c = h.component[i];
            w(
                JPEG + 0x24 + i * 4,
                u32::from(c.id) << 16
                    | u32::from(c.h) << 12
                    | u32::from(c.v) << 8
                    | u32::from(c.quant),
            );
        }
        for class in 0..2 {
            for id in 0..2 {
                let t = &h.huffman[class][id];
                let total_port = JPEG + 0x58 + id * 16 + class * 8;
                let min_port = JPEG + 0x78 + id * 8 + class * 4;
                let mut code = 0u32;
                let mut total = 0;
                for i in 0..16 {
                    total += u32::from(t.counts[i]);
                    w(total_port, total);
                    w(
                        min_port,
                        if t.counts[i] == 0 {
                            0xffff
                        } else {
                            code << (15 - i)
                        },
                    );
                    code = (code + u32::from(t.counts[i])) << 1;
                }
                for symbol in &t.symbols[..if class == 0 { 16 } else { 256 }] {
                    w(total_port + 4, u32::from(*symbol));
                }
            }
        }
        w(JPEG + 0x3c, 0);
        w(JPEG + 0x44, u32::MAX);
    }
    fn configure_dma(
        h: &Header,
        tx_desc: u32,
        rx_desc: u32,
        conversion: ColorConversion,
        timer: &mut Timer,
    ) -> (u32, u32, u32, bool) {
        w(DMA + 0xa04, 4);
        pulse(DMA + 0xa04, 3);
        w(DMA + 0xa18, 0);
        w(DMA + 0xa1c, 0);
        // TX no reorder. RX0 reorder places MCU blocks into a row-major picture.
        w(DMA, (1 << 1) | (1 << 2) | (2 << 6) | (3 << 9) | (1 << 12));
        let block = match (h.component[0].h, h.component[0].v) {
            (2, 2) => 2,
            (2, 1) => 1,
            _ => 0,
        };
        w(
            RX,
            (1 << 2) | (2 << 6) | (block << 9) | (1 << 12) | (1 << 16),
        );
        w(DMA + 0x38, 0);
        w(RX + 0x3c, 0); // JPEG TX/RX trigger0
        w(DMA + 8, 0);
        w(RX + 8, 0);
        w(DMA + 0x10, 0x1fff);
        w(RX + 0x10, 0x3fff);
        if h.components == 1 {
            w(RX + 0x4c, 7 << 3 | 1);
            w(RX + 0x50, 0);
        } else {
            // Programmable CSC. No post-scramble gives native little-endian RGB565;
            // the IDF RGB-order example uses BYTE2_0_1 and returns big-endian bytes.
            w(RX + 0x4c, 1 << 2);
            w(RX + 0x50, 0);
            let matrix: [[i32; 4]; 3] = match conversion {
                ColorConversion::JpegFull => [
                    [256, 0, 359, -45952],
                    [256, -88, -183, 34688],
                    [256, 454, 0, -58112],
                ],
                ColorConversion::Bt601Limited => [
                    [298, 0, 409, -56906],
                    [298, -100, -208, 34707],
                    [298, 516, 0, -70836],
                ],
            };
            for (i, [a, b, c, d]) in matrix.into_iter().enumerate() {
                w(
                    RX + 0x54 + i * 8,
                    (a as u32 & 1023) | ((b as u32 & 2047) << 10),
                );
                w(
                    RX + 0x58 + i * 8,
                    (c as u32 & 1023) | ((d as u32 & 0x3ffff) << 10),
                );
            }
        }
        w(DMA + 0x20, tx_desc);
        w(RX + 0x20, rx_desc);
        fence();
        let axi_ready = r(DMA + 0xa00);
        w(DMA + 0x1c, 1 << 21);
        fence();
        // rev1.3: simultaneous RX descriptor fetch and outstanding TX reads
        // reproduce RID_ERR_CNT. Prefill TX, then drain read commands before RX.
        // This is a measured startup workaround, not a fixed timing delay.
        let begin = timer.now_us();
        let mut ready = true;
        while (r(DMA + 4) & 1 == 0 && r(DMA + 0x14) & (1 << 23) != 0)
            || r(DMA + 0xa00) & 0x0007_f000 != 0
        {
            if timer.now_us() - begin >= 100_000 {
                ready = false;
                break;
            }
        }
        let axi_tx = r(DMA + 0xa00);
        fence(); // order the readiness reads before publishing RX START
        if ready {
            w(RX + 0x1c, 1 << 22);
        }
        fence();
        (axi_ready, axi_tx, r(DMA + 0xa00), ready)
    }
    /// # Safety
    /// HP0 owns JPEG and all 2D-DMA channels exclusively, no concurrent PPA/codec.
    /// Descriptor workspace >=128 bytes, 64-byte aligned, CPU-owned until return.
    /// Buffers in owned HP SRAM or initialized uncached PSRAM, D-cache OFF/L2=0.
    /// Input is immutable until this returns;
    /// Header must be the unmodified parse result of this exact input.
    /// Output is not LCD/DMA-owned. Driver quiesces hardware before releasing slices.
    pub unsafe fn decode(
        input: &[u8],
        output: &mut [u8],
        h: &Header,
        descriptors: &mut [u8],
        conversion: ColorConversion,
        timer: &mut Timer,
    ) -> Result<Snapshot, Error> {
        if h.sos >= input.len() || h.components == 0 || h.stride == 0 || h.rows == 0 {
            return Err(Error::Memory);
        }
        let cache = crate::hal::memory::cache_snapshot();
        if cache.l1_bypass & 16 == 0 || cache.l2_size != 0 {
            return Err(Error::Cache);
        }
        let source = physical(input.as_ptr() as usize, input.len())?;
        let target = physical(output.as_ptr() as usize, output.len())?;
        if output.len() < h.output_bytes() {
            return Err(Error::Memory);
        }
        let address = descriptors.as_mut_ptr() as usize;
        if descriptors.len() < 128 || address & 63 != 0 {
            return Err(Error::Memory);
        }
        let desc_bus =
            if address >= 0x4ff00000 && address.checked_add(128).is_some_and(|e| e <= 0x4ffc0000) {
                address as u32
            } else {
                physical(address, 128)?
            };
        let _claim = Claim::acquire().ok_or(Error::Busy)?;
        let old_clock = r(CLK);
        modify(CLK, 0, (1 << 6) | (1 << 8));
        pulse(RESET, (1 << 30) | (1 << 31));
        let axi_reset = r(DMA + 0xa00);
        configure(h);
        let len = (input.len() - h.sos) as u32;
        let tx = [
            (len & 0x3fff) | ((len & 0x3fff) << 14) | (1 << 31),
            (len >> 14) | ((len >> 14) << 14) | (1 << 28),
            0,
            source + h.sos as u32,
            0,
            0,
        ];
        let hb = if h.components == 1 {
            96
        } else {
            match (h.component[0].h, h.component[0].v) {
                (1, 1) => 32,
                (2, 1) => 64,
                _ => 48,
            }
        };
        let hb = hb.min(u32::from(h.stride));
        let rx = [
            (u32::from(h.component[0].v) * 8) | (hb << 14) | (1 << 29) | (1 << 31),
            u32::from(h.rows)
                | (u32::from(h.stride) << 14)
                | ((if h.components == 1 { 1 } else { 3 }) << 28),
            1 << 28,
            target,
            0,
            0,
        ];
        for (offset, words) in [(0, tx), (64, rx)] {
            for (i, v) in words.into_iter().enumerate() {
                w(address + offset + i * 4, v);
            }
        }
        fence();
        let (axi_ready, axi_tx, axi_rx, startup_ready) =
            configure_dma(h, desc_bus, desc_bus + 64, conversion, timer);
        if startup_ready {
            modify(JPEG, 0, 1 << 1);
        }
        fence();
        let start = timer.now_us();
        let mut result = loop {
            let mut state = snapshot();
            state.axi_reset = axi_reset;
            state.axi_ready = axi_ready;
            state.axi_tx = axi_tx;
            state.axi_rx = axi_rx;
            // All AXI error counters, including RID, are fatal. Never mask the
            // symptom that motivated the serialized startup workaround.
            if state.jpeg & 0x01fc_e3fc != 0
                || state.rx & 0x3ffc != 0
                || state.tx & 4 != 0
                || (state.axi | axi_ready | axi_tx | axi_rx) & 0xfff != 0
            {
                break Err(Error::Hardware(state));
            }
            if state.rx & 2 != 0 {
                break Ok(state);
            }
            if !startup_ready || timer.now_us() - start > 1_000_000 {
                break Err(Error::Timeout(state));
            }
            crate::worker::poll();
        };
        if result.is_ok() {
            // RX EOF can precede retirement of the last AXI write commands.
            // Never reset write FIFOs or release the destination while pending.
            let drain = timer.now_us();
            while r(DMA + 0xa00) & 0x03f8_0000 != 0 {
                assert!(
                    timer.now_us() - drain < 100_000,
                    "JPEG AXI writes did not drain; keep destination reserved"
                );
            }
            fence();
            if let Ok(ref mut state) = result {
                state.axi_drained = r(DMA + 0xa00);
            }
        }
        let axi_aborted = stop(timer);
        match &mut result {
            Ok(s) | Err(Error::Hardware(s)) | Err(Error::Timeout(s)) => s.axi_aborted = axi_aborted,
            _ => {}
        }
        if let Ok(s) = &result {
            if (s.axi_drained | s.axi_aborted) & 0xfff != 0 {
                result = Err(Error::Hardware(*s));
            }
        }
        modify(CLK, (1 << 6) | (1 << 8), old_clock & ((1 << 6) | (1 << 8)));
        fence();
        result
    }
}
#[cfg(all(target_arch = "riscv32", feature = "jpeg-lab"))]
pub use hardware::decode;
