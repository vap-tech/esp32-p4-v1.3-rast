/* ESP32-P4 HP TCM: executable 8-KiB SRAM, no cache/MMU setup required. */
OUTPUT_ARCH(riscv)
ENTRY(_start)
MEMORY {
  RAM (rwx) : ORIGIN = 0x30100000, LENGTH = 8K
  SRAM (rw) : ORIGIN = 0x4ff00000, LENGTH = 16K
}
SECTIONS
{
  .text : ALIGN(16) { KEEP(*(.text.entry)); *(.text .text.*); } > RAM
  .rodata : ALIGN(4) { *(.rodata .rodata.*); } > RAM
  .data : ALIGN(4) { *(.data .data.* .sdata .sdata.*); } > RAM
  .bss (NOLOAD) : ALIGN(4) {
    _bss_start = .;
    *(.bss .bss.* .sbss .sbss.*); *(COMMON);
    . = ALIGN(4);
    _bss_end = .;
  } > SRAM
  _stack_top = ORIGIN(SRAM) + LENGTH(SRAM);
  ASSERT(_bss_end <= _stack_top - 8192, "No space for stack")
  /DISCARD/ : { *(.eh_frame .eh_frame_hdr); }
}
