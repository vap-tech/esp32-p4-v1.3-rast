//! Fixed-role HP1 integer worker. HP0 alone owns drivers and foreground ports.
//! Shared queues/status/HP1 stack use only uncached SRAM. No allocator on HP1.
#[cfg(all(
    target_arch = "riscv32",
    feature = "hp-worker",
    not(any(feature = "multicore-lab", feature = "l2-128"))
))]
mod hardware;
#[cfg(all(
    target_arch = "riscv32",
    feature = "hp-worker",
    not(any(feature = "multicore-lab", feature = "l2-128"))
))]
pub use hardware::*;

/// A generation-matched PAUSE acknowledgement is required; an old PAUSED
/// state is insufficient while a resume or a newer pause is in flight.
fn maintenance_permitted(state: u32, request: u32, ack: u32) -> bool {
    state == 0 || (state == 2 && request & 1 != 0 && request == ack)
}
#[cfg(not(all(
    target_arch = "riscv32",
    feature = "hp-worker",
    not(any(feature = "multicore-lab", feature = "l2-128"))
)))]
pub fn cache_maintenance_allowed() -> bool {
    maintenance_permitted(0, 0, 0)
}
#[cfg(not(all(
    target_arch = "riscv32",
    feature = "hp-worker",
    not(any(feature = "multicore-lab", feature = "l2-128"))
)))]
pub fn poll() {}
#[cfg(not(all(
    target_arch = "riscv32",
    feature = "hp-worker",
    not(any(feature = "multicore-lab", feature = "l2-128"))
)))]
pub fn interrupt() -> bool {
    false
}
#[cfg(not(all(
    target_arch = "riscv32",
    feature = "hp-worker",
    not(any(feature = "multicore-lab", feature = "l2-128"))
)))]
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
    fn cache_permission_requires_exact_live_pause_generation() {
        assert!(maintenance_permitted(0, 0, 0));
        for state in [1, 3, 4] {
            assert!(!maintenance_permitted(state, 5, 5));
        }
        assert!(maintenance_permitted(2, 5, 5));
        assert!(!maintenance_permitted(2, 6, 5)); // resume in flight
        assert!(!maintenance_permitted(2, 7, 5)); // stale pause acknowledgement
        assert!(!maintenance_permitted(2, 6, 6)); // running acknowledgement
    }
}
