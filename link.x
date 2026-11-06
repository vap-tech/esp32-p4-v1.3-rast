/* ESP32-P4 rev 1.x. Keep clear of ROM shared buffers at 0x4ff296b8+. */
OUTPUT_ARCH(riscv)
ENTRY(_start)
MEMORY {
  ENTRY (rx) : ORIGIN = 0x30100000, LENGTH = 8K
  CODE (rx) : ORIGIN = 0x4ff00000, LENGTH = 132K
  DATA (rw) : ORIGIN = 0x4ff21000, LENGTH = 28K
  /* Default: L2=0, physically test upper 512 KiB before use. build.rs's
     l2-128 feature shortens INTERNAL to 368K and moves STACK to 0x4ff9c000,
     excluding the upper 128K cache banks. ROM's 0x4ff296b8..0x4ff40000 is preserved. */
  INTERNAL (rw) : ORIGIN = 0x4ff40000, LENGTH = 496K
  STACK (rw) : ORIGIN = 0x4ffbc000, LENGTH = 16K
}
SECTIONS
{
  .entry : ALIGN(16) { KEEP(*(.text.entry)); } > ENTRY
  .text : ALIGN(16) { *(.text .text.*); } > CODE
  .rodata : ALIGN(4) { *(.rodata .rodata.*); } > CODE
  .data : ALIGN(4) { *(.data .data.* .sdata .sdata.*); } > DATA
  .bss (NOLOAD) : ALIGN(4) {
    _bss_start = .;
    *(.bss .bss.* .sbss .sbss.*); *(COMMON);
    . = ALIGN(4);
    _bss_end = .;
  } > DATA
  .boot_audit (NOLOAD) : ALIGN(8) {
    KEEP(*(.boot_audit));
    . = ALIGN(8);
    _boot_static_end = .;
  } > DATA
  _boot_stack_top = ORIGIN(DATA) + LENGTH(DATA);
  _stack_bottom = ORIGIN(STACK);
  _stack_top = ORIGIN(STACK) + LENGTH(STACK);
  _internal_start = ORIGIN(INTERNAL);
  _internal_end = ORIGIN(INTERNAL) + LENGTH(INTERNAL);
  ASSERT(_boot_stack_top - 8192 == 0x4ff26000, "Retired boot scratch moved")
  ASSERT(_boot_static_end <= _boot_stack_top - 8192, "No space for boot stack")
  /DISCARD/ : { *(.eh_frame .eh_frame_hdr); }
}
