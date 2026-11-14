//! LP timer service: autonomous reset-entry ticks in LP SRAM, HP stays awake.
const MASK48: u64 = (1u64 << 48) - 1;
pub const MAGIC: u32 = 0x4c505431;
pub fn interval_ticks(hz: u32, period_ms: u32) -> Option<u32> {
    if !(1000..=1_000_000).contains(&hz) || !(100..=60_000).contains(&period_ms) {
        return None;
    }
    u32::try_from((u64::from(hz) * u64::from(period_ms) + 500) / 1000)
        .ok()
        .filter(|n| *n != 0)
}
pub fn next_target(now: u64, interval: u32) -> u64 {
    now.wrapping_add(u64::from(interval)) & MASK48
}
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub sequence: u32,
    pub boots: u32,
    pub wake: u32,
    pub timer_raw: u32,
    pub ticks: u64,
    pub next: u64,
    pub interval: u32,
    pub misa: u32,
}
impl Snapshot {
    pub fn decode(first: u32, last: u32, words: [u32; 10]) -> Option<Self> {
        if first & 1 != 0 || first != last {
            return None;
        }
        let check =
            words[0] ^ words[1] ^ words[2] ^ words[3] ^ words[4] ^ words[5] ^ words[6] ^ MAGIC;
        if words[9] != check || words[4] > 0xffff || words[6] > 0xffff {
            return None;
        }
        Some(Self {
            sequence: first,
            boots: words[0],
            wake: words[1],
            timer_raw: words[2],
            ticks: u64::from(words[3]) | (u64::from(words[4]) << 32),
            next: u64::from(words[5]) | (u64::from(words[6]) << 32),
            interval: words[7],
            misa: words[8],
        })
    }
}
#[cfg(all(target_arch = "riscv32", feature = "lp-timer", not(feature = "lp-lab")))]
mod service;
#[cfg(all(target_arch = "riscv32", feature = "lp-timer", not(feature = "lp-lab")))]
pub use service::*;
#[cfg(not(all(target_arch = "riscv32", feature = "lp-timer", not(feature = "lp-lab"))))]
pub fn command(
    _: &str,
    _: &mut crate::hal::time::Timer,
    _: &mut crate::hal::usb_serial::Console,
) -> bool {
    false
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_carries_low_word_and_wraps_48_bits() {
        assert_eq!(next_target(0xffff_fffe, 5), 0x100000003);
        assert_eq!(next_target(MASK48 - 2, 5), 2);
    }
    #[test]
    fn bounded_period_conversion() {
        assert_eq!(interval_ticks(32768, 1000), Some(32768));
        assert_eq!(interval_ticks(136000, 100), Some(13600));
        assert_eq!(interval_ticks(0, 1000), None);
        assert_eq!(interval_ticks(136000, 0), None);
        assert_eq!(interval_ticks(136000, 60001), None);
    }
    #[test]
    fn snapshot_rejects_torn_and_corrupt_data() {
        let mut w = [
            7,
            1 << 18,
            1 << 31,
            0xffff_fff0,
            2,
            100,
            3,
            32768,
            0x40101105,
            0,
        ];
        w[9] = w[..7].iter().fold(MAGIC, |a, b| a ^ b);
        let s = Snapshot::decode(14, 14, w).unwrap();
        assert_eq!(s.boots, 7);
        assert_eq!(s.ticks, 0x2fffffff0);
        assert!(Snapshot::decode(13, 13, w).is_none());
        assert!(Snapshot::decode(14, 16, w).is_none());
        w[0] ^= 1;
        assert!(Snapshot::decode(14, 14, w).is_none());
    }
}
