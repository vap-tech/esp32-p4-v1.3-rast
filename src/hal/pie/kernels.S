# Fixed caller-saved GPRs and exclusive HP1 Q-register ownership.
# a0=output, a1=input A, a2=input B, a3=number of 16-byte blocks.
# Pointers must be 16-byte aligned and disjoint; zero length is allowed.
.section .text.pie,"ax"
.option push
.option norvc
.option norelax
.balign 4
.global pie_init
pie_init:
    li a3,0
    esp.movx.w.cfg a3
    esp.zero.q q0
    esp.zero.q q1
    esp.zero.q q2
    esp.zero.q q3
    esp.zero.q q4
    esp.zero.q q5
    esp.zero.q q6
    esp.zero.q q7
    esp.zero.qacc
    esp.zero.xacc
    ret
.global pie_copy
pie_copy:
    beqz a3,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vst.128.ip q0,a0,16
    addi a3,a3,-1
    bnez a3,1b
2:  ret
.global pie_add8
pie_add8:
    beqz a3,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vld.128.ip q1,a2,16
    esp.vadd.s8 q2,q0,q1
    esp.vst.128.ip q2,a0,16
    addi a3,a3,-1
    bnez a3,1b
2:  ret
.global pie_add16
pie_add16:
    beqz a3,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vld.128.ip q1,a2,16
    esp.vadd.s16 q2,q0,q1
    esp.vst.128.ip q2,a0,16
    addi a3,a3,-1
    bnez a3,1b
2:  ret
.global pie_add32
pie_add32:
    beqz a3,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vld.128.ip q1,a2,16
    esp.vadd.s32 q2,q0,q1
    esp.vst.128.ip q2,a0,16
    addi a3,a3,-1
    bnez a3,1b
2:  ret
.global pie_sub16
pie_sub16:
    beqz a3,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vld.128.ip q1,a2,16
    esp.vsub.s16 q2,q0,q1
    esp.vst.128.ip q2,a0,16
    addi a3,a3,-1
    bnez a3,1b
2:  ret
.global pie_min16
pie_min16:
    beqz a3,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vld.128.ip q1,a2,16
    esp.vmin.s16 q2,q0,q1
    esp.vst.128.ip q2,a0,16
    addi a3,a3,-1
    bnez a3,1b
2:  ret
.global pie_max16
pie_max16:
    beqz a3,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vld.128.ip q1,a2,16
    esp.vmax.s16 q2,q0,q1
    esp.vst.128.ip q2,a0,16
    addi a3,a3,-1
    bnez a3,1b
2:  ret
.global pie_and
pie_and:
    beqz a3,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vld.128.ip q1,a2,16
    esp.andq q2,q0,q1
    esp.vst.128.ip q2,a0,16
    addi a3,a3,-1
    bnez a3,1b
2:  ret
.global pie_or
pie_or:
    beqz a3,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vld.128.ip q1,a2,16
    esp.orq q2,q0,q1
    esp.vst.128.ip q2,a0,16
    addi a3,a3,-1
    bnez a3,1b
2:  ret
.global pie_xor
pie_xor:
    beqz a3,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vld.128.ip q1,a2,16
    esp.xorq q2,q0,q1
    esp.vst.128.ip q2,a0,16
    addi a3,a3,-1
    bnez a3,1b
2:  ret
.global pie_not
pie_not:
    beqz a3,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.notq q2,q0
    esp.vst.128.ip q2,a0,16
    addi a3,a3,-1
    bnez a3,1b
2:  ret
.global pie_dot16
# a0=A, a1=B, a2=number of 16-byte blocks. Return signed XACC in a1:a0.
pie_dot16:
    esp.zero.xacc
    beqz a2,2f
1:
    esp.vld.128.ip q0,a0,16
    esp.vld.128.ip q1,a1,16
    esp.vmulas.s16.xacc q0,q1
    addi a2,a2,-1
    bnez a2,1b
2:
    esp.movx.r.xacc.l a0
    esp.movx.r.xacc.h a1
    ret
.global pie_loop
# Nonzero a0 iterations; six 32-bit instructions per rev1.3 TRM 1.7.1.5. No nesting.
pie_loop:
    li t0,1
    csrw 0x7f1,t0
    li a1,0
    esp.lp.setup 0,a0,3f
    addi a1,a1,1
    addi a1,a1,1
    addi a1,a1,1
    addi a1,a1,1
    addi a1,a1,1
3:  addi a1,a1,1
    mv a0,a1
    csrw 0x7f1,zero
    ret
.global pie_shl32
# a0=out a1=input a2=blocks a3=shift in 0..31
pie_shl32:
    esp.movx.w.sar a3
    beqz a2,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vsl.32 q2,q0
    esp.vst.128.ip q2,a0,16
    addi a2,a2,-1
    bnez a2,1b
2:  ret
.global pie_shr32
# a0=out a1=input a2=blocks a3=shift in 0..31
pie_shr32:
    esp.movx.w.sar a3
    beqz a2,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vsr.u32 q2,q0
    esp.vst.128.ip q2,a0,16
    addi a2,a2,-1
    bnez a2,1b
2:  ret
.global pie_sar32
# a0=out a1=input a2=blocks a3=shift in 0..31
pie_sar32:
    esp.movx.w.sar a3
    beqz a2,2f
1:
    esp.vld.128.ip q0,a1,16
    esp.vsr.s32 q2,q0
    esp.vst.128.ip q2,a0,16
    addi a2,a2,-1
    bnez a2,1b
2:  ret
.global pie_dot8
pie_dot8:
    esp.zero.xacc
    beqz a2,2f
1:
    esp.vld.128.ip q0,a0,16
    esp.vld.128.ip q1,a1,16
    esp.vmulas.s8.xacc q0,q1
    addi a2,a2,-1
    bnez a2,1b
2:
    esp.movx.r.xacc.l a0
    esp.movx.r.xacc.h a1
    ret
.global pie_dot16_fused
# Preload first pair; fused MAC loads the next A while computing the old pair.
# Last pair uses plain MAC, so there is no speculative read past the slice.
pie_dot16_fused:
    esp.zero.xacc
    beqz a2,2f
    esp.vld.128.ip q0,a0,16
    esp.vld.128.ip q1,a1,16
    addi a2,a2,-1
    beqz a2,3f
1:
    esp.vmulas.s16.xacc.ld.ip q0,a0,16,q0,q1
    esp.vld.128.ip q1,a1,16
    addi a2,a2,-1
    bnez a2,1b
3:
    esp.vmulas.s16.xacc q0,q1
2:
    esp.movx.r.xacc.l a0
    esp.movx.r.xacc.h a1
    ret
.option pop
