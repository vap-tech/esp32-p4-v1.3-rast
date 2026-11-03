/* ESP32-P4 rev 1.x. Keep clear of ROM shared buffers at 0x4ff296b8+. */
OUTPUT_ARCH(riscv)
ENTRY(_start)
MEMORY {
  ENTRY (rx) : ORIGIN = 0x30100000, LENGTH = 8K
  CODE (rx) : ORIGIN = 0x4ff00000, LENGTH = 128K
  DATA (rw) : ORIGIN = 0x4ff20000, LENGTH = 32K
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
  _stack_top = ORIGIN(DATA) + LENGTH(DATA);
  ASSERT(_bss_end <= _stack_top - 8192, "No space for stack")
  /DISCARD/ : { *(.eh_frame .eh_frame_hdr); }
}
