/* Integer-only relocatable LP image. No calls, HP memory, or absolute code
 * relocations; trap is fixed at CODE+0x200. MAIL seqlock is external MMIO. */
.section .rodata.lp_timer,"a"
.option push
.option norelax
.option norvc
.balign 512
.global lp_timer_start
lp_timer_start:
    csrci mstatus,8
    csrw mie,zero
    li sp,0x5010a000
    li t0,0x50109200
    csrw mtvec,t0
    li s0,0x50109800
    lw t0,60(s0)
    li t1,0x4c505431
    bne t0,t1,lp_timer_bad_header
    lw t0,0(s0)
    addi t0,t0,1
    sw t0,0(s0)
    fence rw,rw
    lw s1,4(s0)
    addi s1,s1,1
    sw s1,4(s0)
    li s2,0x50115000
    lw s3,0x190(s2)
    sw s3,8(s0)
    li s4,0x50112000
    lw s5,0x38(s4)
    sw s5,12(s0)
    li t1,0x80000000
    sw t1,0x180(s2) /* ack HP startup trigger */
    sw t1,0x44(s4)  /* ack LP TIMER target1 */
    lw t1,0x10(s4)
    li t2,0x10000000
    or t1,t1,t2
    sw t1,0x10(s4)
    fence iorw,iorw
    lw s6,0x14(s4)
    lw s7,0x18(s4)
    slli s7,s7,16
    srli s7,s7,16
    sw s6,16(s0)
    sw s7,20(s0)
    lw t1,32(s0)
    add s8,s6,t1
    sltu t2,s8,s6
    add s9,s7,t2
    slli s9,s9,16
    srli s9,s9,16
    sw s8,24(s0)
    sw s9,28(s0)
    sw s9,0xc(s4)
    sw s8,0x8(s4)
    li t1,0x80000000
    or t1,t1,s9
    sw t1,0xc(s4)
    fence iorw,iorw
    li t1,0x80000000
    sw t1,0x44(s4) /* discard any old deadline before enabling timer wake */
    li t1,0x40000
    sw t1,0x18c(s2) /* from now on ONLY target1 wakes LP */
    fence iorw,iorw
    csrr t1,misa
    sw t1,36(s0)
    xor t1,s1,s3
    xor t1,t1,s5
    xor t1,t1,s6
    xor t1,t1,s7
    xor t1,t1,s8
    xor t1,t1,s9
    li t2,0x4c505431
    xor t1,t1,t2
    sw t1,40(s0)
    fence rw,rw
    lw t0,0(s0)
    addi t0,t0,1
    sw t0,0(s0)
    fence rw,rw
    li t0,0x80000000
    sw t0,0x188(s2)
    fence iorw,iorw
1:  wfi
    j 1b
lp_timer_bad_header:
    ebreak
.balign 512
    li t0,0x50109800
    csrr t1,mcause
    sw t1,48(t0)
    csrr t1,mepc
    sw t1,52(t0)
    csrr t1,mtval
    sw t1,56(t0)
    fence rw,rw
    li t1,1
    sw t1,44(t0)
    li t0,0x5011518c
    sw zero,0(t0)
    fence iorw,iorw
2:  wfi
    j 2b
.balign 4
.global lp_timer_end
lp_timer_end:
.option pop
