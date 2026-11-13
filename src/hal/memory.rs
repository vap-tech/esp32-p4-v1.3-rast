//! Memory/cache configuration readback for ESP32-P4 rev1.x.
const CACHE: usize = 0x3ff1_0000;
#[derive(Debug)]
pub struct CacheSnapshot {
    pub l1_i_ctrl: u32,
    pub l1_d_ctrl: u32,
    pub l1_bypass: u32,
    pub l1_d_size: u32,
    pub l1_d_line: u32,
    pub l2_ctrl: u32,
    pub l2_bypass: u32,
    pub l2_size: u32,
    pub l2_line: u32,
    pub l2_attributes: u32,
}
pub fn cache_snapshot() -> CacheSnapshot {
    let read = |offset| unsafe { ((CACHE + offset) as *const u32).read_volatile() };
    CacheSnapshot {
        l1_i_ctrl: read(0),
        l1_d_ctrl: read(4),
        l1_bypass: read(8),
        l1_d_size: read(0x18),
        l1_d_line: read(0x1c),
        l2_ctrl: read(0x270),
        l2_bypass: read(0x274),
        l2_size: read(0x278),
        l2_line: read(0x27c),
        l2_attributes: read(0x3bc),
    }
}

pub const UPPER_BASE: usize = 0x4ff4_0000;
pub const DMA_START: usize = 0x4ff0_0000;
pub const DMA_END: usize = if cfg!(feature = "wifi") {
    0x4ff1_d000
} else {
    0x4ff2_2000
};
pub const DMA_FIXED_END: usize = 0x4ff2_6000;

#[cfg(target_arch = "riscv32")]
extern "C" {
    static _entry_start: u8;
    static _entry_end: u8;
    static _upper_start: u8;
    static _upper_text_end: u8;
    static _upper_rodata_end: u8;
    static _upper_load_end: u8;
    static _bss_start: u8;
    static _bss_end: u8;
    static _internal_start: u8;
    static _internal_end: u8;
    static _stack_bottom: u8;
    static _stack_top: u8;
    static _dma_start: u8;
    static _dma_end: u8;
}
// Linker aliases may share an address. Keep LLVM from treating distinct
// extern statics as necessarily distinct Rust objects when checking boundaries.
#[cfg(target_arch = "riscv32")]
#[inline]
fn symbol_address(symbol: *const u8) -> usize {
    core::hint::black_box(symbol as usize)
}
/// First free byte after stage-2 code, constants and statics (64-byte aligned).
pub fn internal_start() -> usize {
    #[cfg(target_arch = "riscv32")]
    {
        symbol_address(core::ptr::addr_of!(_internal_start))
    }
    #[cfg(not(target_arch = "riscv32"))]
    {
        UPPER_BASE
    }
}
#[derive(Debug)]
pub struct Layout {
    pub bootstrap_bytes: usize,
    pub runtime_start: usize,
    pub text_bytes: usize,
    pub rodata_bytes: usize,
    pub data_bytes: usize,
    pub bss_bytes: usize,
    pub work_start: usize,
    pub work_bytes: usize,
    pub dma_bytes: usize,
    pub stack_bytes: usize,
}
#[cfg(target_arch = "riscv32")]
pub fn layout() -> Layout {
    let start = symbol_address(core::ptr::addr_of!(_upper_start));
    let text = symbol_address(core::ptr::addr_of!(_upper_text_end));
    let rodata = symbol_address(core::ptr::addr_of!(_upper_rodata_end));
    let end = symbol_address(core::ptr::addr_of!(_upper_load_end));
    let bss = symbol_address(core::ptr::addr_of!(_bss_start));
    let bss_end = symbol_address(core::ptr::addr_of!(_bss_end));
    Layout {
        bootstrap_bytes: symbol_address(core::ptr::addr_of!(_entry_end))
            - symbol_address(core::ptr::addr_of!(_entry_start)),
        runtime_start: start,
        text_bytes: text - start,
        rodata_bytes: rodata - text,
        data_bytes: end - rodata,
        bss_bytes: bss_end - bss,
        work_start: internal_start(),
        work_bytes: STACK_START - internal_start(),
        dma_bytes: DMA_END - DMA_START,
        stack_bytes: STACK_BYTES,
    }
}
pub const L2_SIZE_REGISTER: u32 = if cfg!(feature = "l2-128") { 0x200 } else { 0 };
pub const INTERNAL_END: usize = if cfg!(feature = "l2-128") {
    0x4ffa_0000
} else {
    0x4ffc_0000
};
pub const STACK_BYTES: usize = 64 * 1024;
pub const STACK_START: usize = INTERNAL_END - STACK_BYTES;
pub const STACK_PATTERN: u32 = 0xa55a_6996;
#[derive(Debug)]
pub enum Error {
    CacheLayout(u32),
    RegionLayout,
    Mismatch {
        address: usize,
        expected: u32,
        actual: u32,
    },
}
/// Check the configured SRAM/cache partition and initialize the runtime stack.
/// No writes to the arena or image data; the bring-up destructive test is retired.
/// # Safety
/// Single core, interrupts disabled. Call once on the boot stack before switching
/// SP to the reserved runtime stack. No other owner may access that stack yet.
pub unsafe fn init_runtime_memory() -> Result<(), Error> {
    let cache = cache_snapshot();
    if cache.l2_size != L2_SIZE_REGISTER
        || cache.l1_bypass & 0x13 != 0x13
        || cache.l2_bypass & 0x20 == 0
    {
        return Err(Error::CacheLayout(cache.l2_size));
    }
    #[cfg(target_arch = "riscv32")]
    {
        let start = symbol_address(core::ptr::addr_of!(_upper_start));
        let load_end = symbol_address(core::ptr::addr_of!(_upper_load_end));
        let bss_start = symbol_address(core::ptr::addr_of!(_bss_start));
        let bss_end = symbol_address(core::ptr::addr_of!(_bss_end));
        let linked = [
            symbol_address(core::ptr::addr_of!(_internal_end)),
            symbol_address(core::ptr::addr_of!(_stack_bottom)),
            symbol_address(core::ptr::addr_of!(_stack_top)),
            symbol_address(core::ptr::addr_of!(_dma_start)),
            symbol_address(core::ptr::addr_of!(_dma_end)),
        ];
        if linked != [STACK_START, STACK_START, INTERNAL_END, DMA_START, DMA_END]
            || start < UPPER_BASE
            || load_end < start
            || bss_start < load_end
            || bss_end < bss_start
            || internal_start() != bss_end
            || !internal_start().is_multiple_of(64)
            || internal_start() >= STACK_START
        {
            return Err(Error::RegionLayout);
        }
    }
    // Global bypass means neither alias can retain dirty lines. Initialize the
    // separately reserved stack before switching SP; leave all other SRAM intact.
    unsafe {
        for address in (STACK_START..INTERNAL_END).step_by(4) {
            (address as *mut u32).write_volatile(STACK_PATTERN);
        }
        core::arch::asm!("fence rw, rw", options(nostack));
    }
    Ok(())
}
#[derive(Debug)]
pub struct StackUsage {
    pub reserved: usize,
    pub high_water: usize,
    pub guard_ok: bool,
}
/// Read the boot-filled stack watermark. Diagnostic, not a hardware stack limit.
pub fn stack_usage() -> StackUsage {
    let mut first = STACK_START;
    while first < INTERNAL_END && unsafe { (first as *const u32).read_volatile() } == STACK_PATTERN
    {
        first += 4;
    }
    let guard_ok = (STACK_START..STACK_START + 64)
        .step_by(4)
        .all(|a| unsafe { (a as *const u32).read_volatile() } == STACK_PATTERN);
    StackUsage {
        reserved: STACK_BYTES,
        high_water: INTERNAL_END - first,
        guard_ok,
    }
}

pub fn valid_ahb_range(address: usize, bytes: usize) -> bool {
    bytes != 0
        && address.is_multiple_of(64)
        && bytes.is_multiple_of(64)
        && address.checked_add(bytes).is_some_and(|end| {
            (address >= DMA_START && end <= DMA_FIXED_END)
                || (address >= UPPER_BASE && end <= INTERNAL_END)
        })
}
/// # Safety
/// init_runtime_memory must have succeeded; call exactly once. Stack and
/// ROM areas are excluded. No other owner may ever access this arena's range.
pub unsafe fn internal_arena() -> crate::memory::Arena<'static, crate::memory::Internal> {
    unsafe {
        crate::memory::Arena::from_raw(
            core::ptr::NonNull::new_unchecked(internal_start() as *mut u8),
            STACK_START - internal_start(),
        )
    }
}
/// Separate low SRAM pool, disjoint from runtime payload, work arena and boot scratch.
/// # Safety
/// Call once after init_runtime_memory. No other owner may access this interval.
pub unsafe fn dma_arena() -> crate::memory::Arena<'static, crate::memory::Internal> {
    unsafe {
        crate::memory::Arena::from_raw(
            core::ptr::NonNull::new_unchecked(DMA_START as *mut u8),
            DMA_END - DMA_START,
        )
    }
}
/// Ordering-only handoff for the current uncached policy. Deliberately refuse
/// a later cached policy until its writeback/invalidation path is implemented.
pub(crate) fn sync_internal(
    address: usize,
    bytes: usize,
    _invalidate: bool,
    _timer: &mut super::time::Timer,
) -> Result<(), Error> {
    let cache = cache_snapshot();
    if !valid_ahb_range(address, bytes)
        || cache.l1_bypass & (1 << 4) == 0
        || cache.l2_bypass & (1 << 5) == 0
    {
        return Err(Error::CacheLayout(address as u32));
    }
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dma_bounds_alignment_and_overflow() {
        assert!(valid_ahb_range(DMA_START, DMA_END - DMA_START));
        assert!(valid_ahb_range(0x4ff4_0000, 2048));
        for (a, n) in [
            (0x8800_0000, 2048),
            (0x4ff4_0001, 2048),
            (0x4ff4_0000, 2047),
            (INTERNAL_END, 64),
            (usize::MAX - 63, 128),
            (0x4ff4_0000, 0),
            (DMA_FIXED_END, 64), // Boot stack / legacy CPU scratch is not DMA pool.
            (DMA_FIXED_END - 64, 128),
            (0x4ff2_8000, 64), // Reserved ROM interval.
        ] {
            assert!(!valid_ahb_range(a, n));
        }
    }
}

#[derive(Debug)]
pub struct Benchmark {
    pub size: usize,
    pub passes: usize,
    pub write_us: u64,
    pub read_us: u64,
    pub dependent_steps: usize,
    pub dependent_us: u64,
    pub checksum: u32,
}
fn elapsed(timer: &mut super::time::Timer, start: u64) -> u64 {
    timer.now_us().wrapping_sub(start) & ((1 << 48) - 1)
}
/// Uncached CPU microbenchmark: volatile u32 sequential accesses and dependent
/// loads following an LCG permutation. Includes instruction/loop overhead.
/// # Safety
/// Own and initialize the complete aligned scratch range; no DMA/other users.
/// All caches must be bypassed. Does not change global cache state or aliases.
#[inline(never)]
pub unsafe fn benchmark(
    address: usize,
    capacity: usize,
    timer: &mut super::time::Timer,
    mut report: impl FnMut(Benchmark),
) -> Result<(), Error> {
    let cache = cache_snapshot();
    if cache.l1_bypass & 0x13 != 0x13 || cache.l2_bypass & 0x20 == 0 {
        return Err(Error::CacheLayout(cache.l2_size));
    }
    let ptr = address as *mut u32;
    for size in [64usize, 2048, 16384, 131072, 1048576] {
        if size > capacity {
            continue;
        }
        let words = size / 4;
        let passes = (1048576 / size).clamp(2, 4096);
        let start = timer.now_us();
        for _ in 0..passes {
            for i in 0..words {
                unsafe {
                    ptr.add(i).write_volatile(i as u32 ^ 0xa593_c67e);
                }
            }
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
        let write_us = elapsed(timer, start);
        for i in 0..words {
            let actual = unsafe { ptr.add(i).read_volatile() };
            let expected = i as u32 ^ 0xa593_c67e;
            if actual != expected {
                return Err(Error::Mismatch {
                    address: address + i * 4,
                    expected,
                    actual,
                });
            }
        }
        let mut checksum = 0u32;
        let start = timer.now_us();
        for _ in 0..passes {
            for i in 0..words {
                checksum = checksum.wrapping_add(unsafe { ptr.add(i).read_volatile() });
            }
        }
        let read_us = elapsed(timer, start);
        let mask = words as u32 - 1;
        for i in 0..words {
            unsafe {
                ptr.add(i).write_volatile(
                    (i as u32).wrapping_mul(1664525).wrapping_add(1013904223) & mask,
                );
            }
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
        let dependent_steps = 65536;
        let mut at = 0u32;
        let start = timer.now_us();
        for _ in 0..dependent_steps {
            at = unsafe { ptr.add(at as usize).read_volatile() } & mask;
        }
        let dependent_us = elapsed(timer, start);
        checksum ^= at;
        report(Benchmark {
            size,
            passes,
            write_us,
            read_us,
            dependent_steps,
            dependent_us,
            checksum,
        });
    }
    Ok(())
}

/// Toggle core0 instruction cache only; preserve the data/L2 configuration.
/// # Safety
/// Single active core, interrupts disabled, immutable executable SRAM, no other
/// cache maintenance in progress. Runs from SPM throughout the transition.
#[cfg(target_arch = "riscv32")]
#[link_section = ".text.entry"]
#[inline(never)]
pub unsafe fn set_instruction_cache(enabled: bool) -> Result<(), Error> {
    let read = |offset| unsafe { ((CACHE + offset) as *const u32).read_volatile() };
    let write = |offset, value| unsafe { ((CACHE + offset) as *mut u32).write_volatile(value) };
    if read(0x278) != L2_SIZE_REGISTER {
        return Err(Error::CacheLayout(read(0x278)));
    }
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack));
    }
    write(8, read(8) | 1);
    write(0x9c, 1); // Only core0 I-cache, full-range invalidate.
    write(0xa0, 0);
    write(0xa4, 0);
    write(0x98, 1);
    let mut spins = 0;
    while read(0x98) & 16 == 0 {
        spins += 1;
        if spins > 1_000_000 {
            return Err(Error::CacheLayout(read(0x98)));
        }
    }
    if enabled {
        write(8, read(8) & !1);
    }
    unsafe {
        core::arch::asm!("fence rw, rw", "fence.i", options(nostack));
    }
    Ok(())
}

/// Reset/enable core0 I/D and L2 I/D counters (interrupts remain disabled).
pub fn instruction_counts_start() {
    unsafe {
        ((CACHE + 0x17c) as *mut u32).write_volatile(0x110011);
        ((CACHE + 0x2fc) as *mut u32).write_volatile(0x11001100);
    }
}
/// Stop counters: hits, misses, conflicts, next-level reads; raw hardware units.
pub fn instruction_counts_stop() -> [u32; 4] {
    unsafe {
        ((CACHE + 0x17c) as *mut u32).write_volatile(0);
        ((CACHE + 0x2fc) as *mut u32).write_volatile(0);
    }
    core::array::from_fn(|i| unsafe { ((CACHE + 0x180 + i * 4) as *const u32).read_volatile() })
}

/// Raw data/L2 counters. Order per bank: hit, miss, conflict, next-level-read.
pub fn extra_cache_counts() -> [[u32; 4]; 3] {
    [0x1c0usize, 0x300, 0x340].map(|base| {
        core::array::from_fn(|i| unsafe { ((CACHE + base + i * 4) as *const u32).read_volatile() })
    })
}

/// Toggle L2 only in the separately linked 128 KiB layout; D-cache must be off.
/// # Safety
/// Single core, interrupts disabled, no DMA or external cached data in use.
#[cfg(target_arch = "riscv32")]
#[link_section = ".text.entry"]
#[inline(never)]
pub unsafe fn set_l2_cache(enabled: bool) -> Result<(), Error> {
    let read = |offset| unsafe { ((CACHE + offset) as *const u32).read_volatile() };
    let write = |offset, value| unsafe { ((CACHE + offset) as *mut u32).write_volatile(value) };
    if L2_SIZE_REGISTER != 0x200 || read(0x278) != 0x200 || read(8) & 0x10 == 0 {
        return Err(Error::CacheLayout(read(0x278)));
    }
    // No cached external data is used by this diagnostic configuration.
    write(0x274, 0x20);
    write(0x9c, 32);
    write(0xa0, 0);
    write(0xa4, 0);
    write(0x98, 1);
    let mut spins = 0;
    while read(0x98) & 16 == 0 {
        spins += 1;
        if spins > 1_000_000 {
            return Err(Error::CacheLayout(read(0x98)));
        }
    }
    if enabled {
        write(0x274, 0);
    }
    unsafe {
        core::arch::asm!("fence rw, rw", "fence.i", options(nostack));
    }
    Ok(())
}

/// Toggle L1 D-cache only at L2=0, preserving all dirty internal data on exit.
/// # Safety
/// Single core, interrupts disabled, no active DMA; future DMA needs maintenance.
/// This transition must remain stackless in the release disassembly.
#[cfg(target_arch = "riscv32")]
#[link_section = ".text.entry"]
#[inline(never)]
pub unsafe fn set_data_cache(enabled: bool) -> Result<(), Error> {
    let read = |offset| unsafe { ((CACHE + offset) as *const u32).read_volatile() };
    let write = |offset, value| unsafe { ((CACHE + offset) as *mut u32).write_volatile(value) };
    if read(0x278) != 0 || read(0x274) & 0x20 == 0 {
        return Err(Error::CacheLayout(read(0x278)));
    }
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack));
    }
    write(0x9c, 16);
    write(0xa0, 0);
    write(0xa4, 0);
    if read(8) & 0x10 == 0 {
        // Match ESP-IDF's P4 ROM patch: writeback twice before bypassing.
        for _ in 0..2 {
            write(0x98, 4);
            let mut spins = 0;
            while read(0x98) & 16 == 0 {
                spins += 1;
                if spins > 1_000_000 {
                    return Err(Error::CacheLayout(read(0x98)));
                }
            }
        }
    }
    write(8, read(8) | 16);
    write(0x98, 1);
    let mut spins = 0;
    while read(0x98) & 16 == 0 {
        spins += 1;
        if spins > 1_000_000 {
            return Err(Error::CacheLayout(read(0x98)));
        }
    }
    if enabled {
        write(8, read(8) & !16);
    }
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack));
    }
    Ok(())
}

/// Positive control for L2: read a stable framebuffer line via both PSRAM aliases.
/// # Safety
/// PSRAM/framebuffer initialized, no writer (camera stopped), D-cache bypassed;
/// no cached external allocations. Only LCD scanout may read concurrently.
#[cfg(target_arch = "riscv32")]
#[link_section = ".text.entry"]
#[inline(never)]
pub unsafe fn probe_l2() -> Result<([u32; 4], u32, u32), Error> {
    let read = |offset| unsafe { ((CACHE + offset) as *const u32).read_volatile() };
    let write = |offset, value| unsafe { ((CACHE + offset) as *mut u32).write_volatile(value) };
    if read(0x278) != 0x200 || read(0x274) & 32 != 0 || read(8) & 16 == 0 {
        return Err(Error::CacheLayout(read(0x278)));
    }
    // Previous diagnostic external reads are never dirty; discard cached alias.
    write(0x9c, 32);
    write(0xa0, 0);
    write(0xa4, 0);
    write(0x98, 1);
    let mut spins = 0;
    while read(0x98) & 16 == 0 {
        spins += 1;
        if spins > 1_000_000 {
            return Err(Error::CacheLayout(read(0x98)));
        }
    }
    let mut expected = 0u32;
    for i in 0..256 {
        expected =
            expected.wrapping_add(unsafe { (0x8800_0080 as *const u32).add(i).read_volatile() });
    }
    write(0x2fc, 0x10001000); // L2 DBUS0 reset+enable.
    let mut actual = 0u32;
    for _ in 0..4 {
        for i in 0..256 {
            actual =
                actual.wrapping_add(unsafe { (0x4800_0080 as *const u32).add(i).read_volatile() });
        }
    }
    write(0x2fc, 0);
    let counts = [read(0x340), read(0x344), read(0x348), read(0x34c)];
    // Leave no cached external alias behind for subsequent framebuffer writes.
    write(0x98, 1);
    spins = 0;
    while read(0x98) & 16 == 0 {
        spins += 1;
        if spins > 1_000_000 {
            return Err(Error::CacheLayout(read(0x98)));
        }
    }
    Ok((counts, actual, expected.wrapping_mul(4)))
}
