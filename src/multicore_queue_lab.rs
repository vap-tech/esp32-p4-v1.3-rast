//! Boot diagnostic for queues and bidirectional IRQs. Requires a dedicated image.
//! No web, filesystem, peripheral or allocator ownership is given to HP1.
use core::{
    arch::{asm, global_asm},
    fmt::Write,
    mem::MaybeUninit,
    sync::atomic::{AtomicU32, Ordering},
};
use esp32_p4::{
    hal::{multicore, time::Timer, usb_serial::Console},
    intercore::{Consumer, Producer, Queue},
};
const N: usize = 8;
struct Job {
    id: u32,
    seed: u32,
    rounds: u32,
    stop: bool,
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
    receive: multicore::Receiver,
}
static mut PORTS: MaybeUninit<Ports> = MaybeUninit::uninit();
#[repr(C, align(64))]
struct Status([AtomicU32; 16]);
#[no_mangle]
static HPQ_STATUS: Status = Status([const { AtomicU32::new(0) }; 16]);
#[repr(C, align(16))]
struct Stack([u32; 2048]);
#[no_mangle]
static mut HPQ_STACK: Stack = Stack([0; 2048]);
fn uncached<T>(p: *mut T) -> *mut T {
    (core::hint::black_box(p as usize) + 0x40000000) as *mut T
}
fn status() -> &'static Status {
    unsafe { &*uncached(core::ptr::addr_of!(HPQ_STATUS).cast_mut()) }
}
unsafe fn read(a: usize) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
unsafe fn write(a: usize, value: u32) {
    unsafe {
        (a as *mut u32).write_volatile(value);
        asm!("fence iorw,iorw", options(nostack));
    }
}

global_asm!(
    r#"
.section .text.hpq,"ax"
.option push
.option norelax
.balign 4
.global hpq_entry
hpq_entry:
    csrci mstatus, 8
    csrw mie, zero
    la sp, HPQ_STACK
    li t0, 0x40002000
    add sp, sp, t0
    la t0, HPQ_STATUS
    li t1, 0x40000000
    add t0,t0,t1
    sw sp,8(t0)
    csrr t1,mhartid
    sw t1,12(t0)
    la t0,hpq_trap
    ori t0,t0,3
    csrw mtvec,t0
    call hpq_worker
1:
    wfi
    j 1b
.balign 64
.global hpq_trap
hpq_trap:
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
    la t0,HPQ_STATUS
    li t1,0x40000000
    add t0,t0,t1
    csrr t1,mhartid
    beqz t1,2f
    li t1,0x500e5010
    sw zero,0(t1)
    addi t0,t0,4
    j 4f
2:
    li t1,0x500e5014
    sw zero,0(t1)
    addi t0,t0,28
4:
    fence iorw,iorw
    lw t1,0(t0)
    addi t1,t1,1
    sw t1,0(t0)
    lw t0,0(sp)
    lw t1,4(sp)
    lw t2,8(sp)
    addi sp,sp,16
    mret
3:
    csrci mstatus,8
    la t0,HPQ_STATUS
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
5:
    wfi
    j 5b
.option pop
"#
);

fn calculate(seed: u32, rounds: u32) -> u32 {
    let mut v = seed;
    for _ in 0..rounds {
        v ^= v << 13;
        v ^= v >> 17;
        v ^= v << 5;
    }
    v
}
#[no_mangle]
extern "C" fn hpq_worker() -> ! {
    // Published before ROM entry; read/move once, only through uncached alias.
    let mut ports = unsafe {
        uncached(core::ptr::addr_of_mut!(PORTS))
            .read()
            .assume_init()
    };
    unsafe {
        for i in 0..48 {
            write(0x20801000 + i * 4, 0);
        }
        write(0x20800000, 3 << 1);
        write(0x20800008, 0x1f000000);
        ports.receive.acknowledge();
        write(ports.receive.route_address(1).unwrap(), 18);
        write(0x20801048, 0x3fc00100);
        asm!("csrsi mstatus,8", options(nostack));
    }
    status().0[0].store(1, Ordering::Release);
    loop {
        let Some(job) = ports.jobs.try_pop() else {
            unsafe {
                multicore::wait_if(|| {
                    if ports.jobs.is_empty() {
                        let count = status().0[8].load(Ordering::Relaxed);
                        status().0[8].store(count.wrapping_add(1), Ordering::Relaxed);
                        true
                    } else {
                        false
                    }
                });
            }
            continue;
        };
        let reply = Reply {
            id: job.id,
            value: calculate(job.seed, job.rounds),
        };
        let mut pending = reply;
        while let Err(back) = ports.replies.try_push(pending) {
            pending = back;
            core::hint::spin_loop();
        }
        ports.notify.notify();
        if job.stop {
            unsafe {
                asm!("csrci mstatus,8", options(nostack));
                write(0x20801048, 0);
                write(ports.receive.route_address(1).unwrap(), 0);
                ports.receive.acknowledge();
            }
            status().0[0].store(2, Ordering::Release);
            loop {
                unsafe {
                    asm!("wfi", options(nostack));
                }
            }
        }
    }
}

// Scoped HP0 diagnostic IRQ environment. Not used together with SD/runtime IRQs.
struct HostIrqs {
    clic: [u32; 48],
    config: u32,
    threshold: u32,
    route: u32,
    mtvec: usize,
    mstatus: usize,
}
impl HostIrqs {
    unsafe fn enter(rx: &mut multicore::Receiver) -> Self {
        extern "C" {
            fn hpq_trap();
        }
        let (mtvec, mstatus): (usize, usize);
        unsafe {
            asm!("csrr {0},mtvec", "csrr {1},mstatus", "csrci mstatus,8",out(reg)mtvec,out(reg)mstatus,options(nostack));
            let mut clic = [0; 48];
            for (i, v) in clic.iter_mut().enumerate() {
                *v = read(0x20801000 + i * 4);
                write(0x20801000 + i * 4, *v & !(1 << 8));
            }
            let this = Self {
                clic,
                config: read(0x20800000),
                threshold: read(0x20800008),
                route: read(rx.route_address(0).unwrap()),
                mtvec,
                mstatus,
            };
            rx.acknowledge();
            write(rx.route_address(0).unwrap(), 18);
            write(0x20800000, (this.config & !0x1e) | (3 << 1));
            write(0x20800008, 0x1f000000);
            write(0x20801048, 0x3fc00100);
            let trap = hpq_trap as *const () as usize | 3;
            asm!("csrw mtvec,{0}","csrsi mstatus,8",in(reg)trap,options(nostack));
            this
        }
    }
}
impl Drop for HostIrqs {
    fn drop(&mut self) {
        unsafe {
            asm!("csrci mstatus,8", options(nostack));
            write(0x20801048, 0);
            write(0x500e5014, 0);
            write(0x500d6140, self.route);
            write(0x20800000, self.config);
            write(0x20800008, self.threshold);
            for (i, v) in self.clic.iter().enumerate() {
                write(0x20801000 + i * 4, *v);
            }
            asm!("csrw mtvec,{0}","csrw mstatus,{1}",in(reg)self.mtvec,in(reg)self.mstatus,options(nostack));
        }
    }
}

/// # Safety
/// Once, HP0 boot only, L1 bypass, before SD/runtime IRQs. Own HP1 and IRQ
/// sources0/1. No source/queue/stack/ports are reclaimed, including on timeout.
pub unsafe fn run(timer: &mut Timer, console: &mut Console) {
    extern "C" {
        fn hpq_entry() -> !;
    }
    timer.delay_us(3_000_000);
    #[cfg(feature = "multicore-cache-lab")]
    unsafe {
        esp32_p4::hal::memory::set_instruction_cache(true).unwrap();
        esp32_p4::hal::memory::set_instruction_cache_for(1, true).unwrap();
        esp32_p4::hal::memory::set_data_cache(true).unwrap();
    }
    let _ = writeln!(
        console,
        "HP QUEUE cache {:?}",
        esp32_p4::hal::memory::cache_snapshot()
    );
    let (mut jobs, job_rx) = unsafe { (&mut *uncached(core::ptr::addr_of_mut!(JOBS))).split() };
    let (reply_tx, mut replies) =
        unsafe { (&mut *uncached(core::ptr::addr_of_mut!(REPLIES))).split() };
    let (mut notify, receive) = unsafe { multicore::claim(0) };
    let (reply_notify, mut reply_receive) = unsafe { multicore::claim(1) };
    let stack = uncached(core::ptr::addr_of_mut!(HPQ_STACK)).cast::<u32>();
    unsafe {
        for i in 0..2048 {
            stack.add(i).write_volatile(0xa55a6996);
        }
        uncached(core::ptr::addr_of_mut!(PORTS)).write(MaybeUninit::new(Ports {
            jobs: job_rx,
            replies: reply_tx,
            notify: reply_notify,
            receive,
        }));
    }
    let irqs = unsafe { HostIrqs::enter(&mut reply_receive) };
    unsafe {
        multicore::start_hp1(hpq_entry);
    }
    let start = timer.now_us();
    while status().0[0].load(Ordering::Acquire) == 0 && timer.now_us() - start < 500_000 {
        core::hint::spin_loop();
    }
    unsafe {
        multicore::clear_rom_entry();
    }
    // Burst fill/drain and idle periods exercise coalescing and wakeup races.
    let mut sent = 0;
    let mut received = 0;
    let mut correct = true;
    let mut backpressure = 0u32;
    while received < 4096
        && timer.now_us() - start < 5_000_000
        && status().0[0].load(Ordering::Acquire) == 1
    {
        let before = sent;
        while sent < 4096 {
            let job = Job {
                id: sent,
                seed: sent ^ 0x9e3779b9,
                rounds: 256,
                stop: false,
            };
            if jobs.try_push(job).is_err() {
                backpressure += 1;
                break;
            }
            sent += 1;
        }
        if sent != before {
            notify.notify();
        }
        while let Some(reply) = replies.try_pop() {
            if reply.id != received || reply.value != calculate(received ^ 0x9e3779b9, 256) {
                correct = false;
            }
            received += 1;
        }
        if received.is_multiple_of(64) {
            timer.delay_us(1000);
        }
    }
    let stop = Job {
        id: u32::MAX,
        seed: 0,
        rounds: 0,
        stop: true,
    };
    let stop_sent = jobs.try_push(stop).is_ok();
    notify.notify();
    let end = timer.now_us();
    while status().0[0].load(Ordering::Acquire) == 1 && timer.now_us() - end < 500_000 {
        let _ = replies.try_pop();
    }
    let parked = status().0[0].load(Ordering::Acquire) == 2;
    unsafe {
        multicore::stall_hp1();
    }
    drop(irqs);
    let guard = unsafe { stack.read_volatile() == 0xa55a6996 };
    let passed = sent == 4096
        && received == 4096
        && correct
        && stop_sent
        && parked
        && guard
        && status().0[7].load(Ordering::Acquire) > 0
        && status().0[1].load(Ordering::Acquire) > 0
        && status().0[8].load(Ordering::Acquire) > 0
        && status().0[4].load(Ordering::Acquire) == 0;
    let _ = writeln!(
        console,
        "HP QUEUE passed={} wfi_entries={}",
        passed,
        status().0[8].load(Ordering::Acquire)
    );
    let _=writeln!(console,"HP QUEUE sent={} received={} correct={} stop={} parked={} irq_hp0={} irq_hp1={} backpressure={} stack_guard={} trap={:08x}/{:08x}/{:08x}",sent,received,correct,stop_sent,parked,status().0[7].load(Ordering::Acquire),status().0[1].load(Ordering::Acquire),backpressure,unsafe{stack.read_volatile()==0xa55a6996},status().0[4].load(Ordering::Acquire),status().0[5].load(Ordering::Acquire),status().0[6].load(Ordering::Acquire));
    if !parked {
        // Do not enter normal boot/cache transitions without software proof
        // that HP1 has stopped touching shared state. Recovery is a reset.
        let _ = writeln!(
            console,
            "HP QUEUE NOT QUIESCENT: boot halted; reset required"
        );
        loop {
            core::hint::spin_loop();
        }
    }
    #[cfg(feature = "multicore-cache-lab")]
    unsafe {
        // No cache maintenance until HP1 has stopped accessing shared state.
        esp32_p4::hal::memory::set_data_cache(false).unwrap();
        esp32_p4::hal::memory::set_instruction_cache_for(1, false).unwrap();
        esp32_p4::hal::memory::set_instruction_cache(false).unwrap();
    }
}
