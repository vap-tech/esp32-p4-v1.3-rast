//! ESP32-P4 rev1.x PIE 2.1. No RVV and no automatic context switching.
//! Assembly is kept beside vendor-verified words so stock LLVM builds it.
//! Buffers currently require the 0x4ff SRAM path; PSRAM is not validated.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block(pub [u8; 16]);

#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub enum Operation {
    Copy,
    Add8,
    Add16,
    Add32,
    Sub16,
    Min16,
    Max16,
    And,
    Or,
    Xor,
    Not,
}
pub const OPERATIONS: [Operation; 11] = [
    Operation::Copy,
    Operation::Add8,
    Operation::Add16,
    Operation::Add32,
    Operation::Sub16,
    Operation::Min16,
    Operation::Max16,
    Operation::And,
    Operation::Or,
    Operation::Xor,
    Operation::Not,
];

/// Independent scalar oracle, including saturation at signed limits.
pub fn reference(op: Operation, a: &Block, b: &Block) -> Block {
    let mut out = Block([0; 16]);
    match op {
        Operation::Copy => out = *a,
        Operation::Add8 => {
            for i in 0..16 {
                out.0[i] = (a.0[i] as i8).saturating_add(b.0[i] as i8) as u8;
            }
        }
        Operation::Add32 => {
            for i in (0..16).step_by(4) {
                let x = i32::from_le_bytes(a.0[i..i + 4].try_into().unwrap());
                let y = i32::from_le_bytes(b.0[i..i + 4].try_into().unwrap());
                out.0[i..i + 4].copy_from_slice(&x.saturating_add(y).to_le_bytes());
            }
        }
        Operation::Add16 | Operation::Sub16 | Operation::Min16 | Operation::Max16 => {
            for i in (0..16).step_by(2) {
                let x = i16::from_le_bytes([a.0[i], a.0[i + 1]]);
                let y = i16::from_le_bytes([b.0[i], b.0[i + 1]]);
                let v = match op {
                    Operation::Add16 => x.saturating_add(y),
                    Operation::Sub16 => x.saturating_sub(y),
                    Operation::Min16 => x.min(y),
                    Operation::Max16 => x.max(y),
                    _ => unreachable!(),
                };
                out.0[i..i + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
        Operation::And | Operation::Or | Operation::Xor | Operation::Not => {
            for i in 0..16 {
                out.0[i] = match op {
                    Operation::And => a.0[i] & b.0[i],
                    Operation::Or => a.0[i] | b.0[i],
                    Operation::Xor => a.0[i] ^ b.0[i],
                    Operation::Not => !a.0[i],
                    _ => unreachable!(),
                };
            }
        }
    }
    out
}

// Only the 0x4ff SRAM path has passed the 128-bit load/store probe. The
// 0x8ff direct/debug path trapped on VST despite working for scalar accesses.
#[cfg(any(target_arch = "riscv32", test))]
fn sram_range(address: usize, blocks: usize) -> bool {
    if blocks == 0 {
        return true;
    }
    address & 15 == 0
        && address >= 0x4ff0_0000
        && blocks
            .checked_mul(16)
            .and_then(|n| address.checked_add(n))
            .is_some_and(|end| end <= 0x4ffc_0000)
}

#[cfg(target_arch = "riscv32")]
mod hardware {
    use super::{sram_range, Block, Operation};
    use core::{
        arch::{asm, global_asm},
        marker::PhantomData,
    };
    global_asm!(include_str!("pie/encoded.S"));
    extern "C" {
        fn pie_init();
        fn pie_copy(out: *mut Block, a: *const Block, b: *const Block, blocks: usize);
        fn pie_add8(out: *mut Block, a: *const Block, b: *const Block, blocks: usize);
        fn pie_add16(out: *mut Block, a: *const Block, b: *const Block, blocks: usize);
        fn pie_add32(out: *mut Block, a: *const Block, b: *const Block, blocks: usize);
        fn pie_sub16(out: *mut Block, a: *const Block, b: *const Block, blocks: usize);
        fn pie_min16(out: *mut Block, a: *const Block, b: *const Block, blocks: usize);
        fn pie_max16(out: *mut Block, a: *const Block, b: *const Block, blocks: usize);
        fn pie_and(out: *mut Block, a: *const Block, b: *const Block, blocks: usize);
        fn pie_or(out: *mut Block, a: *const Block, b: *const Block, blocks: usize);
        fn pie_xor(out: *mut Block, a: *const Block, b: *const Block, blocks: usize);
        fn pie_not(out: *mut Block, a: *const Block, b: *const Block, blocks: usize);
        fn pie_dot16(a: *const Block, b: *const Block, blocks: usize) -> u64;
        fn pie_loop(iterations: u32) -> u32;
        fn pie_shl32(out: *mut Block, a: *const Block, blocks: usize, shift: u32);
        fn pie_shr32(out: *mut Block, a: *const Block, blocks: usize, shift: u32);
        fn pie_sar32(out: *mut Block, a: *const Block, blocks: usize, shift: u32);
        fn pie_dot8(a: *const Block, b: *const Block, blocks: usize) -> u64;
        fn pie_dot16_fused(a: *const Block, b: *const Block, blocks: usize) -> u64;
    }
    /// Exclusive HP0 copy-only owner. No ISR may use PIE while held.
    /// Raw DMA memory is accessed only after its driver has acquired ownership.
    pub struct CopyEngine {
        _pinned: PhantomData<*mut ()>,
    }
    impl CopyEngine {
        /// # Safety
        /// HP0 foreground, exclusive PIE ownership; all interrupt handlers must
        /// preserve Q registers by never using PIE. No task migration/context switch.
        /// Keep D-cache OFF and L2=0 for this experimentally validated DMA policy.
        pub unsafe fn claim_hp0() -> Result<Self, &'static str> {
            let (hart, state): (usize, usize);
            unsafe {
                asm!("csrr {0},mhartid",out(reg)hart,options(nostack));
                asm!("csrr {0},0x7f2",out(reg)state,options(nostack));
            }
            let cache = crate::hal::memory::cache_snapshot();
            if hart != 0 || state & 3 != 0 || cache.l1_bypass & 16 == 0 || cache.l2_size != 0 {
                return Err("HP0 PIE already owned or unsupported cache layout");
            }
            unsafe {
                asm!("csrwi 0x7f2,1", options(nostack));
                pie_init();
                asm!("csrwi 0x7f2,2", options(nostack));
            }
            Ok(Self {
                _pinned: PhantomData,
            })
        }
        /// Try the aligned prefix, leaving exact tails/unaligned buffers to the
        /// scalar path. Returns bytes copied, never rounds up or changes aliases.
        /// # Safety
        /// Both regions valid and disjoint for len bytes, ordinary internal SRAM.
        /// Caller owns both regions; DMA must not access them until handoff.
        /// An opaque assembly call performs actual memory transactions (not pure
        /// Rust memcpy); driver fences must still surround DMA OWN transitions.
        pub unsafe fn copy_prefix(&mut self, dst: *mut u8, src: *const u8, len: usize) -> usize {
            let blocks = len / 16;
            if blocks < 4 || !sram_range(dst as usize, blocks) || !sram_range(src as usize, blocks)
            {
                return 0;
            }
            let bytes = blocks * 16;
            assert!(dst as usize + bytes <= src as usize || src as usize + bytes <= dst as usize);
            unsafe {
                pie_copy(dst.cast(), src.cast(), core::ptr::null(), blocks);
            }
            bytes
        }
    }
    impl Drop for CopyEngine {
        fn drop(&mut self) {
            unsafe {
                asm!("csrwi 0x7f2,0", options(nostack));
            }
        }
    }
    /// Pinned to HP1, !Send + !Sync. Drop disables PIE; it does not save Q state.
    pub struct Pie {
        _pinned: PhantomData<*mut ()>,
    }
    fn check(blocks: &[Block]) {
        assert!(sram_range(blocks.as_ptr() as usize, blocks.len()), "PIE supports only aligned 0x4ff SRAM slices; direct/debug alias and PSRAM are not validated");
    }
    impl Pie {
        /// # Safety
        /// HP1 foreground only, exclusive PIE/HWLOOP owner for this core.
        /// Interrupts/exceptions must not use these extensions or switch tasks.
        /// CPU/cache/clock maintenance must wait until this work completes.
        pub unsafe fn claim_hp1() -> Result<Self, &'static str> {
            let (hart, state, loops): (usize, usize, usize);
            unsafe {
                asm!("csrr {0},mhartid",out(reg)hart, options(nostack));
                asm!("csrr {0},0x7f2",out(reg)state, options(nostack));
                asm!("csrr {0},0x7f1",out(reg)loops, options(nostack));
            }
            if hart != 1 || state & 3 != 0 || loops & 3 != 0 {
                return Err("PIE/HWLOOP already owned or wrong hart");
            }
            unsafe {
                asm!("csrwi 0x7f2,1", options(nostack));
                pie_init();
                asm!("csrwi 0x7f2,2", options(nostack));
            }
            Ok(Self {
                _pinned: PhantomData,
            })
        }
        pub fn state(&self) -> u32 {
            let state;
            unsafe {
                asm!("csrr {0},0x7f2",out(reg)state,options(nostack));
            }
            state
        }
        pub fn apply(&mut self, op: Operation, out: &mut [Block], a: &[Block], b: &[Block]) {
            assert_eq!(out.len(), a.len());
            assert_eq!(out.len(), b.len());
            check(out);
            check(a);
            check(b);
            let f = match op {
                Operation::Copy => pie_copy,
                Operation::Add8 => pie_add8,
                Operation::Add16 => pie_add16,
                Operation::Add32 => pie_add32,
                Operation::Sub16 => pie_sub16,
                Operation::Min16 => pie_min16,
                Operation::Max16 => pie_max16,
                Operation::And => pie_and,
                Operation::Or => pie_or,
                Operation::Xor => pie_xor,
                Operation::Not => pie_not,
            };
            unsafe {
                f(out.as_mut_ptr(), a.as_ptr(), b.as_ptr(), out.len());
            }
        }
        /// Exact signed dot product; bounded to avoid overflowing signed XACC40.
        pub fn dot16(&mut self, a: &[Block], b: &[Block]) -> i64 {
            assert_eq!(a.len(), b.len());
            assert!(a.len() <= 63, "at most 504 full-range int16 products");
            check(a);
            check(b);
            let raw = unsafe { pie_dot16(a.as_ptr(), b.as_ptr(), a.len()) };
            // XACC.H contains only the upper eight bits; sign-extend 40 bits.
            ((raw << 24) as i64) >> 24
        }
        /// Lane-wise 32-bit shift: mode 0=left, 1=logical right, 2=arithmetic right.
        pub fn shift32(&mut self, mode: usize, out: &mut [Block], a: &[Block], shift: u32) {
            assert_eq!(out.len(), a.len());
            assert!(shift < 32);
            check(out);
            check(a);
            let f = [pie_shl32, pie_shr32, pie_sar32][mode];
            unsafe {
                f(out.as_mut_ptr(), a.as_ptr(), out.len(), shift);
            }
        }
        pub fn dot8(&mut self, a: &[Block], b: &[Block]) -> i64 {
            assert_eq!(a.len(), b.len());
            assert!(a.len() <= 1_000_000);
            check(a);
            check(b);
            let raw = unsafe { pie_dot8(a.as_ptr(), b.as_ptr(), a.len()) };
            ((raw << 24) as i64) >> 24
        }
        pub fn dot16_fused(&mut self, a: &[Block], b: &[Block]) -> i64 {
            assert_eq!(a.len(), b.len());
            assert!(a.len() <= 63);
            check(a);
            check(b);
            let raw = unsafe { pie_dot16_fused(a.as_ptr(), b.as_ptr(), a.len()) };
            ((raw << 24) as i64) >> 24
        }
        /// Six adds per iteration, to test the hardware loop end semantics.
        pub fn loop_probe(&mut self, iterations: u32) -> u32 {
            assert!(iterations > 0 && iterations <= u32::MAX / 6);
            unsafe { pie_loop(iterations) }
        }
    }
    impl Drop for Pie {
        fn drop(&mut self) {
            unsafe {
                asm!("csrwi 0x7f2,0", options(nostack));
            }
        }
    }
}
#[cfg(target_arch = "riscv32")]
pub use hardware::{CopyEngine, Pie};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranges_reject_direct_alias_unaligned_and_overflow() {
        assert!(sram_range(0x4ff0_0000, 1));
        assert!(sram_range(0x4ffb_fff0, 1));
        assert!(!sram_range(0x4ffb_fff0, 2));
        assert!(!sram_range(0x8ff4_0000, 1));
        assert!(!sram_range(0x4ff4_0001, 1));
        assert!(!sram_range(0x4ff4_0000, usize::MAX));
        assert!(sram_range(16, 0));
    }
    #[test]
    fn signed_saturation_is_not_wrapping_addition() {
        let a = Block([0x7f; 16]);
        let b = Block([1; 16]);
        assert_eq!(reference(Operation::Add8, &a, &b), a);
        for (op, x, y, want) in [
            (Operation::Add16, 32767i16, 1i16, 32767i16),
            (Operation::Sub16, -32768, 1, -32768),
        ] {
            let mut a = Block([0; 16]);
            let mut b = a;
            for i in (0..16).step_by(2) {
                a.0[i..i + 2].copy_from_slice(&x.to_le_bytes());
                b.0[i..i + 2].copy_from_slice(&y.to_le_bytes());
            }
            assert_eq!(&reference(op, &a, &b).0[..2], &want.to_le_bytes());
        }
        let a = Block([0xff, 0xff, 0xff, 0x7f, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let b = Block([1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            &reference(Operation::Add32, &a, &b).0[..4],
            &i32::MAX.to_le_bytes()
        );
    }
}
