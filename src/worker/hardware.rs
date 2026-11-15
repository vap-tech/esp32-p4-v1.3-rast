#[cfg(feature = "pie-lab")]
#[path = "pie_lab.rs"]
mod pie_lab;
use crate::{
    hal::{self, multicore, time::Timer, usb_serial::Console},
    intercore::{Consumer, Producer, Queue},
};
use core::{
    arch::{asm, global_asm},
    cell::UnsafeCell,
    fmt::Write,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
const N: usize = 8;
const SEED: u32 = 0x9e3779b9;
const ROUNDS: u32 = 4096;
struct Job {
    id: u32,
    seed: u32,
    rounds: u32,
}
struct Reply {
    id: u32,
    value: u32,
}
static mut JOBS: Queue<Job, N> = Queue::new();
static mut REPLIES: Queue<Reply, N> = Queue::new();
struct Ports {
    jobs: Consumer<'static, Job, N>,
    replies: Producer<'static, Reply, N>,
    notify: multicore::Sender,
}
static mut PORTS: MaybeUninit<Ports> = MaybeUninit::uninit();
#[repr(C, align(64))]
struct Status([AtomicU32; 16]);
#[no_mangle]
static HPW_STATUS: Status = Status([const { AtomicU32::new(0) }; 16]);
#[repr(C, align(16))]
struct Stack([u32; 2048]);
#[no_mangle]
static mut HPW_STACK: Stack = Stack([0; 2048]);
fn alias<T>(p: *mut T) -> *mut T {
    (core::hint::black_box(p as usize) + 0x40000000) as *mut T
}
fn status() -> &'static Status {
    unsafe { &*alias(core::ptr::addr_of!(HPW_STATUS).cast_mut()) }
}
unsafe fn write(a: usize, v: u32) {
    unsafe {
        (a as *mut u32).write_volatile(v);
        asm!("fence iorw,iorw", options(nostack));
    }
}
unsafe fn read(a: usize) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
struct Host {
    jobs: Producer<'static, Job, N>,
    replies: Consumer<'static, Reply, N>,
    notify: multicore::Sender,
    submitted: u64,
    completed: u64,
    errors: u32,
    expected: u32,
    load: bool,
}
struct Local(UnsafeCell<MaybeUninit<Host>>);
// HP0 foreground only. ISR touches status atomics, never this object. BUSY
// prevents nested foreground poll/command from forming overlapping borrows.
unsafe impl Sync for Local {}
static HOST: Local = Local(UnsafeCell::new(MaybeUninit::uninit()));
static READY: AtomicBool = AtomicBool::new(false);
static BUSY: AtomicBool = AtomicBool::new(false);
struct Borrow;
impl Borrow {
    fn take() -> Option<Self> {
        let hart: usize;
        unsafe {
            asm!("csrr {0},mhartid",out(reg)hart,options(nostack));
        }
        assert_eq!(hart, 0);
        BUSY.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self)
    }
    fn host(&mut self) -> &mut Host {
        unsafe { (&mut *HOST.0.get()).assume_init_mut() }
    }
}
impl Drop for Borrow {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}
#[inline(never)]
fn calculate(mut v: u32, n: u32) -> u32 {
    for _ in 0..n {
        v ^= v << 13;
        v ^= v >> 17;
        v ^= v << 5;
    }
    v
}

// Only t0/t1/t2 are touched by the HP1 handler. No Rust, FP or PIE in ISR.
global_asm!(
    r#"
.section .text.hpw,"ax"
.option push
.option norelax
.balign 4
.global hpw_entry
hpw_entry:
    csrci mstatus,8
    csrw mie,zero
    la sp,HPW_STACK
    li t0,0x40002000
    add sp,sp,t0
    la t0,HPW_STATUS
    li t1,0x40000000
    add t0,t0,t1
    sw sp,8(t0)
    csrr t1,mhartid
    sw t1,12(t0)
    la t0,hpw_trap
    ori t0,t0,3
    csrw mtvec,t0
    call hpw_worker
1:  wfi
    j 1b
.balign 64
hpw_trap:
    addi sp,sp,-16
    sw t0,0(sp)
    sw t1,4(sp)
    sw t2,8(sp)
    csrr t1,mcause
    bgez t1,3f
    li t2,0xfff
    and t1,t1,t2
    li t2,18
    bne t1,t2,3f
    li t1,0x500e5010
    sw zero,0(t1)
    fence iorw,iorw
    la t0,HPW_STATUS
    li t1,0x40000000
    add t0,t0,t1
    lw t1,4(t0)
    addi t1,t1,1
    sw t1,4(t0)
    lw t0,0(sp)
    lw t1,4(sp)
    lw t2,8(sp)
    addi sp,sp,16
    mret
3:  csrci mstatus,8
    la t0,HPW_STATUS
    li t1,0x40000000
    add t0,t0,t1
    csrr t1,mcause
    sw t1,16(t0)
    csrr t1,mepc
    sw t1,20(t0)
    csrr t1,mtval
    sw t1,24(t0)
    fence rw,rw
    li t1,3
    sw t1,0(t0)
4:  wfi
    j 4b
.option pop
"#
);
// PAUSED code executes entirely in SPM. No stack accesses, no calls, no SRAM
// instruction fetches. a0 is the uncached status pointer. CPU0 can maintain
// caches only with the matching odd request/ack generation and state=2.
global_asm!(
    r#"
.section .text.entry,"ax"
.option push
.option norelax
.balign 4
.global hpw_pause
hpw_pause:
    csrci mstatus,8
1:  li t0,0x500e5010
    sw zero,0(t0)
    fence iorw,iorw
    lw t1,32(a0)
    andi t2,t1,1
    beqz t2,3f
    li t2,2
    sw t2,0(a0)
    fence rw,rw
    sw t1,44(a0)
    fence rw,rw
    lw t2,32(a0)
    bne t1,t2,1b
    wfi
    j 1b
3:  fence.i
    li t2,1
    sw t2,0(a0)
    fence rw,rw
    sw t1,44(a0)
    fence rw,rw
    csrsi mstatus,8
    ret
.option pop
"#
);
#[no_mangle]
extern "C" fn hpw_worker() -> ! {
    let mut ports = unsafe { alias(core::ptr::addr_of_mut!(PORTS)).read().assume_init() };
    unsafe {
        for i in 0..48 {
            write(0x20801000 + i * 4, 0);
        }
        write(0x20800000, 3 << 1);
        write(0x20800008, 0x1f000000);
        write(0x500e5010, 0);
        write(0x500d693c, 18);
        write(0x20801048, 0x3fc00100);
        asm!("csrsi mstatus,8", options(nostack));
    }
    status().0[0].store(1, Ordering::Release);
    loop {
        if status().0[8].load(Ordering::Acquire) & 1 != 0 {
            extern "C" {
                fn hpw_pause(p: *const Status);
            }
            unsafe {
                hpw_pause(status());
            }
        }
        #[cfg(feature = "pie-lab")]
        pie_lab::service();
        let Some(job) = ports.jobs.try_pop() else {
            unsafe {
                multicore::wait_if(|| {
                    #[cfg(feature = "pie-lab")]
                    if pie_lab::pending() {
                        return false;
                    }
                    let idle =
                        ports.jobs.is_empty() && status().0[8].load(Ordering::Acquire) & 1 == 0;
                    if idle {
                        let n = status().0[9].load(Ordering::Relaxed);
                        status().0[9].store(n.wrapping_add(1), Ordering::Relaxed);
                    }
                    idle
                });
            }
            continue;
        };
        let mut reply = Reply {
            id: job.id,
            value: calculate(job.seed, job.rounds) ^ job.id,
        };
        while let Err(back) = ports.replies.try_push(reply) {
            reply = back;
            core::hint::spin_loop();
        }
        ports.notify.notify();
    }
}

/// # Safety
/// Once on HP0 before other IRQ owners; HP1 still in ROM, D-cache bypassed,
/// L2=0, executable SRAM immutable. Fatal startup errors require reset.
pub unsafe fn init(timer: &mut Timer, console: &mut Console) -> Result<(), &'static str> {
    assert!(!READY.load(Ordering::Acquire));
    assert_eq!(status().0[0].load(Ordering::Acquire), 0);
    let cache = hal::memory::cache_snapshot();
    if cache.l1_bypass & 16 == 0 || cache.l2_size != 0 {
        return Err("worker cache layout");
    }
    unsafe {
        hal::memory::set_instruction_cache(true).map_err(|_| "I0 cache")?;
        hal::memory::set_instruction_cache_for(1, true).map_err(|_| "I1 cache")?;
    }
    let (jobs, rx) = unsafe { (&mut *alias(core::ptr::addr_of_mut!(JOBS))).split() };
    let (tx, replies) = unsafe { (&mut *alias(core::ptr::addr_of_mut!(REPLIES))).split() };
    let (notify, _rx0) = unsafe { multicore::claim(0) };
    let (notify_back, mut rx1) = unsafe { multicore::claim(1) };
    unsafe {
        let stack = alias(core::ptr::addr_of_mut!(HPW_STACK)).cast::<u32>();
        for i in 0..2048 {
            stack.add(i).write_volatile(0xa55a6996);
        }
        alias(core::ptr::addr_of_mut!(PORTS)).write(MaybeUninit::new(Ports {
            jobs: rx,
            replies: tx,
            notify: notify_back,
        }));
        (*HOST.0.get()).write(Host {
            jobs,
            replies,
            notify,
            submitted: 0,
            completed: 0,
            errors: 0,
            expected: calculate(SEED, ROUNDS),
            load: false,
        });
        // This is the first HP0 IRQ owner after ROM. Do not inherit enabled
        // CLIC sources whose old handlers are no longer installed.
        for i in 0..48 {
            let address = 0x20801000 + i * 4;
            write(address, read(address) & !(1 << 8));
        }
        rx1.acknowledge();
        write(rx1.route_address(0).unwrap(), 18);
        write(0x20800000, (read(0x20800000) & !0x1e) | (3 << 1));
        write(0x20800008, 0x1f000000);
        write(0x20801048, 0x3fc00100);
        let mtvec: usize;
        asm!("csrr {0},mtvec",out(reg)mtvec,options(nostack));
        asm!("csrw mtvec,{0}",in(reg)(mtvec|3),options(nostack));
    }
    status().0[0].store(4, Ordering::Release);
    extern "C" {
        fn hpw_entry() -> !;
    }
    unsafe {
        multicore::start_hp1(hpw_entry);
    }
    let start = timer.now_us();
    while status().0[0].load(Ordering::Acquire) == 4 && timer.now_us() - start < 500_000 {
        core::hint::spin_loop();
    }
    unsafe {
        multicore::clear_rom_entry();
    }
    if status().0[0].load(Ordering::Acquire) != 1 {
        return Err("HP1 start timeout/fault");
    }
    READY.store(true, Ordering::Release);
    unsafe {
        asm!("csrsi mstatus,8", options(nostack));
    }
    let _ = writeln!(
        console,
        "HP WORKER READY hart={} queues=2x8 stack=8192 shared=uncached I0+I1=ON D=OFF load=OFF",
        status().0[3].load(Ordering::Acquire)
    );
    Ok(())
}
fn drain(host: &mut Host) {
    while let Some(reply) = host.replies.try_pop() {
        if reply.id != host.completed as u32 || reply.value != (host.expected ^ reply.id) {
            host.errors = host.errors.wrapping_add(1);
        }
        host.completed += 1;
    }
}
pub fn poll() {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let Some(mut borrow) = Borrow::take() else {
        return;
    };
    let host = borrow.host();
    drain(host);
    if host.load
        && status().0[0].load(Ordering::Acquire) == 1
        && status().0[8].load(Ordering::Acquire) & 1 == 0
    {
        let before = host.submitted;
        while host.submitted - host.completed < N as u64 {
            let job = Job {
                id: host.submitted as u32,
                seed: SEED,
                rounds: ROUNDS,
            };
            if host.jobs.try_push(job).is_err() {
                break;
            }
            host.submitted += 1;
        }
        if host.submitted != before {
            host.notify.notify();
        }
    }
}
pub fn cache_maintenance_allowed() -> bool {
    let s = status();
    super::maintenance_permitted(
        s.0[0].load(Ordering::Acquire),
        s.0[8].load(Ordering::Acquire),
        s.0[11].load(Ordering::Acquire),
    )
}
pub fn interrupt() -> bool {
    if !READY.load(Ordering::Acquire) {
        return false;
    }
    unsafe {
        write(0x500e5014, 0);
    }
    let n = status().0[7].load(Ordering::Relaxed);
    status().0[7].store(n.wrapping_add(1), Ordering::Relaxed);
    true
}
fn control(host: &mut Host, pause: bool, timer: &mut Timer) -> bool {
    let prior = status().0[8].load(Ordering::Acquire);
    let request = (prior & !1).wrapping_add(2) | u32::from(pause);
    status().0[8].store(request, Ordering::Release);
    host.notify.notify();
    let start = timer.now_us();
    while timer.now_us() - start < 500_000 {
        drain(host);
        if status().0[11].load(Ordering::Acquire) == request
            && status().0[0].load(Ordering::Acquire) == if pause { 2 } else { 1 }
        {
            return true;
        }
        if status().0[0].load(Ordering::Acquire) == 3 {
            return false;
        }
    }
    false
}
fn report(host: &Host, console: &mut Console) {
    // Do not race the live Rust stack. SPM pause makes no stack accesses.
    let usage = if cache_maintenance_allowed() {
        let stack = alias(core::ptr::addr_of_mut!(HPW_STACK)).cast::<u32>();
        let mut untouched = 0;
        for i in 0..2048 {
            if unsafe { stack.add(i).read_volatile() } != 0xa55a6996 {
                break;
            }
            untouched += 4;
        }
        Some(8192 - untouched)
    } else {
        None
    };
    let _=writeln!(console,"HP WORKER state={} load={} submitted={} completed={} pending={} errors={} irq0={} irq1={} wfi={} control={}/{} stack_used={:?}/8192 guard={:?} trap={:08x}/{:08x}/{:08x}",status().0[0].load(Ordering::Acquire),host.load,host.submitted,host.completed,host.submitted-host.completed,host.errors,status().0[7].load(Ordering::Acquire),status().0[1].load(Ordering::Acquire),status().0[9].load(Ordering::Acquire),status().0[8].load(Ordering::Acquire),status().0[11].load(Ordering::Acquire),usage,usage.map(|n| n < 8192),status().0[4].load(Ordering::Acquire),status().0[5].load(Ordering::Acquire),status().0[6].load(Ordering::Acquire));
}
pub fn command(text: &str, timer: &mut Timer, console: &mut Console) -> bool {
    let Some(args) = text.strip_prefix("system worker") else {
        return false;
    };
    if !args.is_empty() && !args.starts_with(' ') {
        return false;
    }
    if !READY.load(Ordering::Acquire) {
        let _ = writeln!(console, "HP WORKER unavailable");
        return true;
    }
    let Some(mut borrow) = Borrow::take() else {
        let _ = writeln!(console, "HP WORKER busy");
        return true;
    };
    let host = borrow.host();
    drain(host);
    if args.trim().starts_with("pie ") {
        #[cfg(feature = "pie-lab")]
        if args.trim() == "pie run"
            && (status().0[0].load(Ordering::Acquire) != 1
                || status().0[8].load(Ordering::Acquire) & 1 != 0)
        {
            let _ = writeln!(
                console,
                "PIE LAB unavailable: HP1 must be running, not paused/faulted"
            );
        } else if pie_lab::command(args.trim(), console) {
            host.notify.notify();
        }
        #[cfg(not(feature = "pie-lab"))]
        let _ = writeln!(
            console,
            "PIE LAB unavailable: rebuild with --features pie-lab"
        );
        report(host, console);
        return true;
    }
    match args.trim() {
        "" | "status" => {}
        "load on" => host.load = true,
        "load off" => host.load = false,
        "pause" | "resume" => {
            let ok = control(host, args.trim() == "pause", timer);
            let _ = writeln!(console, "HP WORKER CONTROL ok={}", ok);
        }
        "cache-check" => {
            let prior: usize;
            unsafe {
                asm!("csrrci {0},mstatus,8", out(reg)prior, options(nostack));
            }
            let result = unsafe { hal::memory::set_instruction_cache_for(1, false) };
            let restore = if result.is_ok() {
                unsafe { hal::memory::set_instruction_cache_for(1, true) }
            } else {
                Ok(())
            };
            if prior & 8 != 0 {
                unsafe {
                    asm!("csrsi mstatus,8", options(nostack));
                }
            }
            let _ = writeln!(
                console,
                "HP WORKER CACHE disable={:?} restore={:?}",
                result, restore
            );
        }
        _ => {
            let _ = writeln!(
                console,
                "HP WORKER commands: status | load on/off | pause | resume | cache-check"
            );
        }
    }
    report(host, console);
    true
}
