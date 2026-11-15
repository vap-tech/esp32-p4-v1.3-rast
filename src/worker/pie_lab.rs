//! HP1-only scratch + release/acquire report publication. No peripheral access.
use crate::hal::{
    pie::{self, Block, Operation, Pie},
    time::cycle_count,
    usb_serial::Console,
};
use core::{
    cell::UnsafeCell,
    fmt::Write,
    sync::atomic::{AtomicU32, Ordering},
};
const BLOCKS: usize = 63;
const REPEATS: usize = 128;
static STATE: AtomicU32 = AtomicU32::new(0); // 0 idle, 1 requested, 2 running, 3 result
static PHASE: AtomicU32 = AtomicU32::new(0);
#[derive(Clone, Copy)]
struct Report {
    errors: [u32; 18],
    scalar: [u64; 5],
    simd: [u64; 5],
    checked: u32,
    initial: u32,
    dirty: u32,
    elapsed: u64,
    released: u32,
    irq: u32,
}
struct Shared(UnsafeCell<Report>);
// HP1 is the sole writer, HP0 reads only after STATE=3 acquire. A new request
// is made by HP0 only after finishing that read. No concurrently shared refs.
unsafe impl Sync for Shared {}
static REPORT: Shared = Shared(UnsafeCell::new(Report {
    errors: [0; 18],
    scalar: [0; 5],
    simd: [0; 5],
    checked: 0,
    initial: 0,
    dirty: 0,
    elapsed: 0,
    released: 0,
    irq: 0,
}));
#[repr(C, align(16))]
struct Buffers {
    a: [Block; BLOCKS],
    b: [Block; BLOCKS],
    before: Block,
    out: [Block; BLOCKS],
    after: Block,
}
struct Scratch(UnsafeCell<Buffers>);
// Only HP1 accesses scratch; D-cache is OFF on this test profile.
// CPU-visible 0x4ff alias for PIE DBUS; never exposed to HP0/DMA.
unsafe impl Sync for Scratch {}
static SCRATCH: Scratch = Scratch(UnsafeCell::new(Buffers {
    a: [Block([0; 16]); BLOCKS],
    b: [Block([0; 16]); BLOCKS],
    before: Block([0xa5; 16]),
    out: [Block([0; 16]); BLOCKS],
    after: Block([0x5a; 16]),
}));
fn alias<T>(p: *mut T) -> *mut T {
    (core::hint::black_box(p as usize) + 0x40000000) as *mut T
}
fn state() -> &'static AtomicU32 {
    unsafe { &*alias(core::ptr::addr_of!(STATE).cast_mut()) }
}
fn phase() -> &'static AtomicU32 {
    unsafe { &*alias(core::ptr::addr_of!(PHASE).cast_mut()) }
}
pub fn pending() -> bool {
    state().load(Ordering::Acquire) == 1
}
pub fn command(args: &str, console: &mut Console) -> bool {
    match args {
        "pie run" => {
            let s = state().load(Ordering::Acquire);
            if s == 0 || s == 3 {
                phase().store(0, Ordering::Relaxed);
                state().store(1, Ordering::Release);
                let _ = writeln!(
                    console,
                    "PIE LAB queued on HP1; use system worker pie status"
                );
            } else {
                let _ = writeln!(console, "PIE LAB busy");
            }
        }
        "pie ping" => {
            return true;
        }
        "pie status" => {
            let s = state().load(Ordering::Acquire);
            let _ = writeln!(
                console,
                "PIE LAB state={} phase={}",
                s,
                phase().load(Ordering::Acquire)
            );
            if s == 3 {
                let r = unsafe { alias(REPORT.0.get()).read() };
                let _=writeln!(console,"PIE LAB checked={} errors={:?} csr={}->{}->{} irq_during={} cycles={} buffers=SRAM D-OFF bytes={} repeats={}",r.checked,r.errors,r.initial,r.dirty,r.released,r.irq,r.elapsed,BLOCKS*16,REPEATS);
                for (i, name) in [
                    "copy-u32",
                    "add16-saturating",
                    "xor-u32",
                    "dot16-i64",
                    "dot16-fused-i64",
                ]
                .iter()
                .enumerate()
                {
                    let _ = writeln!(
                        console,
                        "PIE BENCH {} scalar_cycles={} pie_cycles={}",
                        name, r.scalar[i], r.simd[i]
                    );
                }
            }
        }
        _ => {
            let _ = writeln!(
                console,
                "PIE LAB commands: pie run | pie status | pie ping (IRQ probe)"
            );
        }
    }
    pending()
}
#[inline(never)]
fn scalar(op: usize, out: &mut [Block], a: &[Block], b: &[Block]) -> i64 {
    let n = out.len() * 16;
    let ap = a.as_ptr().cast::<u32>();
    let bp = b.as_ptr().cast::<u32>();
    let dst = out.as_mut_ptr().cast::<u32>();
    unsafe {
        match op {
            0 => {
                for i in 0..n / 4 {
                    dst.add(i).write(ap.add(i).read());
                }
            }
            1 => {
                for i in 0..n / 2 {
                    let x = ap.cast::<i16>().add(i).read();
                    let y = bp.cast::<i16>().add(i).read();
                    dst.cast::<i16>().add(i).write(x.saturating_add(y));
                }
            }
            2 => {
                for i in 0..n / 4 {
                    dst.add(i).write(ap.add(i).read() ^ bp.add(i).read());
                }
            }
            _ => {
                let mut sum = 0i64;
                for i in 0..n / 2 {
                    sum += i64::from(ap.cast::<i16>().add(i).read())
                        * i64::from(bp.cast::<i16>().add(i).read());
                }
                return sum;
            }
        }
    }
    0
}
fn rnd(x: &mut u32) -> u32 {
    *x ^= *x << 13;
    *x ^= *x >> 17;
    *x ^= *x << 5;
    *x
}
pub fn service() {
    if !pending() {
        return;
    }
    state().store(2, Ordering::Release);
    let start = cycle_count();
    let irq_before = super::status().0[1].load(Ordering::Acquire);
    let mut r = Report {
        errors: [0; 18],
        scalar: [u64::MAX; 5],
        simd: [u64::MAX; 5],
        checked: 0,
        initial: 0,
        dirty: 0,
        elapsed: 0,
        released: 0,
        irq: 0,
    };
    let cache = crate::hal::memory::cache_snapshot();
    if cache.l1_bypass & 16 == 0 || cache.l2_size != 0 {
        r.errors[0] = u32::MAX;
        unsafe {
            alias(REPORT.0.get()).write(r);
        }
        state().store(3, Ordering::Release);
        return;
    }
    let Ok(mut p) = (unsafe { Pie::claim_hp1() }) else {
        r.errors[0] = u32::MAX;
        unsafe {
            alias(REPORT.0.get()).write(r);
        }
        state().store(3, Ordering::Release);
        return;
    };
    r.initial = p.state();
    let buf = unsafe { &mut *SCRATCH.0.get() };
    let mut seed = 0x91ab37cd;
    for case in 0..64 {
        phase().store(case + 1, Ordering::Release);
        for i in 0..BLOCKS {
            for j in 0..16 {
                buf.a[i].0[j] = rnd(&mut seed) as u8;
                buf.b[i].0[j] = rnd(&mut seed) as u8;
            }
        }
        // Explicit signed extrema and overflow patterns, not just small signals.
        if case < 8 {
            let patterns: [u32; 8] = [
                0,
                0x7fff_ffff,
                0x8000_0000,
                0x7fff_7fff,
                0x8000_8000,
                0x7f7f_7f7f,
                0x8080_8080,
                0xffff_ffff,
            ];
            for block in &mut buf.a {
                for w in block.0.as_chunks_mut::<4>().0 {
                    w.copy_from_slice(&patterns[case as usize].to_le_bytes());
                }
            }
            for block in &mut buf.b {
                block.0 = [1; 16];
            }
        }
        for (idx, op) in pie::OPERATIONS.into_iter().enumerate() {
            // Length 0, 1, various small tails and full range; Block forbids
            // unaligned pointers, and kernels never read a partial block.
            let len = if case < 8 {
                BLOCKS
            } else {
                case as usize % (BLOCKS + 1)
            };
            buf.out.fill(Block([0xcc; 16]));
            p.apply(op, &mut buf.out[..len], &buf.a[..len], &buf.b[..len]);
            for i in 0..BLOCKS {
                let expected = if i < len {
                    pie::reference(op, &buf.a[i], &buf.b[i])
                } else {
                    Block([0xcc; 16])
                };
                if buf.out[i] != expected {
                    r.errors[idx] += 1;
                }
                r.checked += 1;
            }
            if buf.before != Block([0xa5; 16]) || buf.after != Block([0x5a; 16]) {
                r.errors[idx] += 1;
            }
        }
        let expected = scalar(3, &mut buf.out, &buf.a, &buf.b);
        if p.dot16(&buf.a, &buf.b) != expected {
            r.errors[11] += 1;
        }
        r.checked += 1;
        for len in [0, 1, 7, BLOCKS] {
            let expected = scalar(3, &mut buf.out[..len], &buf.a[..len], &buf.b[..len]);
            if p.dot16_fused(&buf.a[..len], &buf.b[..len]) != expected {
                r.errors[17] += 1;
            }
            let mut expected8 = 0i64;
            for i in 0..len {
                for j in 0..16 {
                    expected8 += i64::from(buf.a[i].0[j] as i8) * i64::from(buf.b[i].0[j] as i8);
                }
            }
            if p.dot8(&buf.a[..len], &buf.b[..len]) != expected8 {
                r.errors[16] += 1;
            }
            r.checked += 2;
        }
        for shift in 0..32 {
            for mode in 0..3 {
                p.shift32(mode, &mut buf.out[..1], &buf.a[..1], shift);
                for lane in 0..4 {
                    let offset = lane * 4;
                    let x = u32::from_le_bytes(buf.a[0].0[offset..offset + 4].try_into().unwrap());
                    let want = match mode {
                        0 => x << shift,
                        1 => x >> shift,
                        _ => ((x as i32) >> shift) as u32,
                    };
                    let got =
                        u32::from_le_bytes(buf.out[0].0[offset..offset + 4].try_into().unwrap());
                    if got != want {
                        r.errors[13 + mode] += 1;
                    }
                    r.checked += 1;
                }
            }
        }
    }
    phase().store(100, Ordering::Release);
    for n in [1, 2, 3, 7, 31, 1000] {
        if p.loop_probe(n) != n * 6 {
            r.errors[12] += 1;
        }
        r.checked += 1;
    }
    phase().store(200, Ordering::Release);
    for round in 0..5 {
        for op in 0..5 {
            // Alternate order, retain the lowest whole-batch time after warmup.
            for order in 0..2 {
                let simd = (order + round) % 2 != 0;
                let begin = cycle_count();
                for _ in 0..REPEATS {
                    core::hint::black_box(&mut buf.out);
                    let sum = if simd {
                        if op == 4 {
                            p.dot16_fused(&buf.a, &buf.b)
                        } else if op == 3 {
                            p.dot16(&buf.a, &buf.b)
                        } else {
                            p.apply(
                                [Operation::Copy, Operation::Add16, Operation::Xor][op],
                                &mut buf.out,
                                &buf.a,
                                &buf.b,
                            );
                            0
                        }
                    } else {
                        scalar(op, &mut buf.out, &buf.a, &buf.b)
                    };
                    core::hint::black_box(sum);
                    core::hint::black_box(&buf.out);
                }
                let cycles = cycle_count() - begin;
                if round > 0 {
                    if simd {
                        r.simd[op] = r.simd[op].min(cycles);
                    } else {
                        r.scalar[op] = r.scalar[op].min(cycles);
                    }
                }
            }
        }
    }
    r.dirty = p.state();
    drop(p);
    unsafe {
        core::arch::asm!("csrr {0},0x7f2",out(reg)r.released,options(nostack));
    }
    r.irq = super::status().0[1]
        .load(Ordering::Acquire)
        .wrapping_sub(irq_before);
    r.elapsed = cycle_count() - start;
    unsafe {
        alias(REPORT.0.get()).write(r);
    }
    phase().store(300, Ordering::Release);
    state().store(3, Ordering::Release);
}
