//! Rev1.3 AXI matrix/VDMA diagnostics. Normal boot policy is not changed.
const ICM: usize = 0x500a_4000;
const CH: usize = 0x5008_1200;
fn read(a: usize) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
fn write(a: usize, v: u32) {
    unsafe { (a as *mut u32).write_volatile(v) }
}
fn field(a: usize, mask: u32, value: u32) {
    write(a, (read(a) & !mask) | value);
}
#[derive(Debug)]
pub struct Snapshot {
    pub icm: [u32; 7], // version, master priority, slave priority, ARQoS, AWQoS, deadline, CPU H2X
    pub qos: [u32; 4], // version, HW configuration, indirect command/data (read only)
    pub vdma: [u32; 7], // CTL0/1, CFG0/1, AXI QoS, global CFG and channel enable
    pub bridge: [u32; 5], // request burst, credit, current depth, low-water, flow control
    pub psram: [u32; 5], // control, clock, AXI control, DDR, split timing
}
pub fn snapshot() -> Snapshot {
    Snapshot {
        icm: [
            read(ICM),
            read(ICM + 0x1c),
            read(ICM + 0x24),
            read(ICM + 0x28),
            read(ICM + 0x30),
            read(ICM + 0x48),
            read(0x500e_5138),
        ],
        qos: core::array::from_fn(|i| read(ICM + 0x400 + i * 4)),
        vdma: [
            read(CH + 0x18),
            read(CH + 0x1c),
            read(CH + 0x20),
            read(CH + 0x24),
            read(CH + 0x58),
            read(0x5008_1010),
            read(0x5008_1018),
        ],
        bridge: [
            read(0x500a_0808),
            read(0x500a_0810),
            read(0x500a_0814),
            read(0x500a_088c),
            read(0x500a_0888),
        ],
        psram: [
            read(0x5008_e00c),
            read(0x5008_e050),
            read(0x5008_e03c),
            read(0x5008_e0d8),
            read(0x5008_e1a0),
        ],
    }
}
/// Restore only fields this experiment can modify, including on error return.
pub struct Restore {
    values: [u32; 6],
    changed: u32,
}
impl Restore {
    /// # Safety
    /// Exclusive matrix/VDMA/PSRAM configuration ownership, other HP core parked.
    pub unsafe fn save() -> Self {
        Self {
            changed: 0,
            values: [
                read(ICM + 0x1c),
                read(ICM + 0x28),
                read(ICM + 0x30),
                read(CH + 0x58),
                read(0x500e_5138),
                read(0x5008_e00c),
            ],
        }
    }
    /// Apply one whitelist profile; no clocks, mapping, reset or format changes.
    /// # Safety
    /// Same ownership as save; channel/clock parameters are not modified live.
    pub unsafe fn apply(&mut self, name: &str) -> Result<(), &'static str> {
        match name {
            "baseline" => {}
            "matrix-prio" => {
                self.changed |= 1 << 0;
                field(ICM + 0x1c, 15 << 16, 15 << 16);
            }
            "matrix-qos" => {
                self.changed |= 1 << 1;
                field(ICM + 0x28, 15 << 16, 15 << 16);
            }
            "wrong-port-qos" => {
                self.changed |= 1 << 1;
                field(ICM + 0x28, 15 << 12, 15 << 12);
            }
            "combined" => {
                self.changed |= (1 << 0) | (1 << 1);
                field(ICM + 0x1c, 15 << 16, 15 << 16);
                field(ICM + 0x28, 15 << 16, 15 << 16);
            }
            "postwrite-off" => {
                self.changed |= 1 << 4;
                field(0x500e_5138, 1, 0);
            }
            "cutthrough-off" => {
                self.changed |= 1 << 4;
                field(0x500e_5138, 2, 0);
            }
            _ => return Err("unknown arbitration profile"),
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
        Ok(())
    }
    fn restore(&self) {
        for (index, (a, v)) in [ICM + 0x1c, ICM + 0x28, ICM + 0x30, CH + 0x58]
            .into_iter()
            .zip(self.values)
            .enumerate()
        {
            if self.changed & (1 << index) != 0 {
                write(a, v);
            }
        }
        if self.changed & (1 << 4) != 0 {
            field(0x500e_5138, 3, self.values[4] & 3);
        }
        if self.changed & (1 << 5) != 0 {
            let mask = (1 << 25) | (1 << 26) | (1 << 29);
            field(0x5008_e00c, mask, self.values[5] & mask);
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
    }
}
impl Drop for Restore {
    fn drop(&mut self) {
        self.restore();
    }
}

// Indirect QoS interface: IDF axi_icm_ll.h; reads do not enable regulation.
fn qos_command(
    port: u32,
    write_channel: bool,
    kind: u32,
    value: Option<u32>,
) -> Result<u32, &'static str> {
    let wait = || {
        let start = crate::hal::time::cycle_count();
        while read(ICM + 0x408) & (1 << 31) != 0 {
            if crate::hal::time::cycle_count().wrapping_sub(start) > 160_000 {
                return Err("QoS command timeout");
            }
        }
        if read(ICM + 0x408) & (1 << 28) != 0 {
            return Err("QoS command rejected");
        }
        Ok(())
    };
    wait()?;
    if let Some(v) = value {
        write(ICM + 0x40c, v);
    }
    write(
        ICM + 0x408,
        (1 << 31)
            | if value.is_some() { 1 << 30 } else { 0 }
            | (port << 8)
            | (u32::from(write_channel) << 7)
            | kind,
    );
    wait()?;
    Ok(read(ICM + 0x40c))
}
pub fn regulator_snapshot() -> Result<[u32; 4], &'static str> {
    if read(ICM + 0x404) & (1 << 7) == 0 {
        return Err("QoS hardware has no bidirectional command interface");
    }
    Ok([
        qos_command(0, true, 0, None)?,
        qos_command(0, true, 1, None)?,
        qos_command(6, false, 0, None)?,
        qos_command(6, false, 1, None)?,
    ])
}
