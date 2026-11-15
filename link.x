/* P4 rev1.x: SPM bootstrap -> Flash payload -> upper SRAM runtime. */
OUTPUT_ARCH(riscv)
ENTRY(_start)
MEMORY {
  ENTRY (rx) : ORIGIN = 0x30100000, LENGTH = 8K
  DMA_RAM (rw) : ORIGIN = 0x4ff00000, LENGTH = 136K
  DMA_FIXED (rw) : ORIGIN = 0x4ff22000, LENGTH = 16K
  BOOT_STACK (rw) : ORIGIN = 0x4ff26000, LENGTH = 8K
  /* 0x4ff28000..0x4ff40000 remains reserved for ROM. */
  UPPER (rwx) : ORIGIN = 0x4ff40000, LENGTH = 432K
  STACK (rw) : ORIGIN = 0x4ffac000, LENGTH = 80K
}
SECTIONS {
  .entry : ALIGN(16) {
    _entry_start = .;
    KEEP(*(.text.entry));
    _entry_end = .;
  } > ENTRY
  .upper : ALIGN(64) {
    _upper_start = .;
    *(.text .text.*);
    _upper_text_end = .;
    . = ALIGN(4);
    *(.rodata .rodata.* .srodata .srodata.*);
    _upper_rodata_end = .;
    . = ALIGN(4);
    *(.data .data.* .sdata .sdata.* .got .got.*);
    . = ALIGN(4);
    _upper_load_end = .;
  } > UPPER
  .dma_fixed (NOLOAD) : ALIGN(64) { _dma_fixed_start = .; *(.dma.fixed); . = ALIGN(64); _dma_fixed_end = .; } > DMA_FIXED
  .bss (NOLOAD) : ALIGN(64) {
    _bss_start = .;
    *(.bss .bss.* .sbss .sbss.*); *(COMMON);
    . = ALIGN(64);
    _bss_end = .;
  } > UPPER
  _boot_stack_top = ORIGIN(BOOT_STACK) + LENGTH(BOOT_STACK);
  _stack_bottom = ORIGIN(STACK);
  _stack_top = ORIGIN(STACK) + LENGTH(STACK);
  _dma_start = ORIGIN(DMA_RAM);
  _dma_end = ORIGIN(DMA_RAM) + LENGTH(DMA_RAM);
  _internal_start = _bss_end;
  _internal_end = ORIGIN(UPPER) + LENGTH(UPPER);
  ASSERT(_boot_stack_top - 8192 == 0x4ff26000, "Retired boot scratch moved")
  ASSERT(_dma_end == 0x4ff22000, "DMA pool overlaps fixed workspaces")
  ASSERT(_dma_fixed_end <= 0x4ff26000, "DMA workspaces overlap boot scratch")
  ASSERT(_upper_start >= 0x4ff40000, "Runtime overlaps ROM")
  ASSERT(_internal_start + 32K <= _stack_bottom, "Less than 32KiB working SRAM")
  ASSERT(_internal_end == _stack_bottom, "Working SRAM overlaps stack")
  /DISCARD/ : { *(.eh_frame .eh_frame_hdr); }
}
