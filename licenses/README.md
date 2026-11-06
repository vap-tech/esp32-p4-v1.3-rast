# Reference source attribution

`src/hal/dsi.rs` adapts register sequences and integer LDO calibration from
Espressif ESP-IDF v6.1 (commit `fff9895c82d744c7237be8847347bdd1b07c6643`),
Copyright 2023–2025 Espressif Systems (Shanghai) CO LTD, Apache-2.0:

- `components/esp_hal_lcd/mipi_dsi_hal.c`
- `components/esp_hal_lcd/esp32p4/include/hal/mipi_dsi{,_host,_phy,_brg}_ll.h`
- `components/esp_hal_pmu/esp32p4/include/hal/ldo_ll.h`
- Register definitions: `components/soc/esp32p4/register/hw_ver1/soc/`.

These were translated to a standalone, restricted Rust implementation. ESP-IDF
is not a build or runtime dependency. The applicable license is included in
[Apache-2.0.txt](Apache-2.0.txt).

Panel reset/MADCTL/DCS-ID sequence was checked against the Apache-2.0
[Espressif JD9165 driver](https://github.com/espressif/esp-iot-solution/blob/master/components/display/lcd/esp_lcd_jd9165/esp_lcd_jd9165.c),
Copyright 2024 Espressif Systems (Shanghai) CO LTD. The board's 50 vendor commands
come from the preserved `old/esp32-p4-voice-device/main/display_test.c`.

`src/hal/ledc.rs` adapts the channel/timer/gamma update sequences from ESP-IDF
v6.1 `components/esp_hal_ledc/esp32p4/include/hal/ledc_ll.h` and
`components/esp_driver_ledc/src/ledc.c`, Copyright 2023–2026 Espressif Systems
(Shanghai) CO LTD, Apache-2.0. PWM arithmetic and the GPIO sampling diagnostic
are implemented locally.

`src/hal/csi.rs` adapts the CSI host/PHY/bridge, DW-GDMA, ISP input bypass
and cache synchronization register sequences from ESP-IDF v6.1, commit above,
Copyright 2023–2026 Espressif Systems (Shanghai) CO LTD, Apache-2.0:

- `components/esp_hal_cam/{mipi_csi_hal.c,esp32p4/include/hal/}`
- `components/esp_driver_cam/csi/src/esp_cam_ctlr_csi.c`
- `components/esp_hal_dma/esp32p4/include/hal/dw_gdma_ll.h`
- `components/esp_driver_isp/src/isp_core.c`
- `components/soc/esp32p4/register/hw_ver1/soc/{cache,dw_gdma,isp,mipi_csi_host,mipi_csi_bridge}_reg.h`

`src/devices/ov02c10.rs` adapts the sensor register table from the open
[esp-video-components PR #46](https://github.com/espressif/esp-video-components/pull/46),
head `ed9f3ddc8bff764b02a0b7c22dd7179b99bdb6a3`,
`esp_cam_sensor/sensors/ov02c10/private_include/ov02c10_settings.h`,
Copyright 2024 Espressif Systems (Shanghai) CO LTD, Apache-2.0.
The crop, polling capture and diagnostic application are local additions.

`src/hal/psram.rs` adapts AP HEX PSRAM/MSPI register settings from ESP-IDF v6.1
`components/esp_psram/device/esp_psram_impl_ap_hex.c` and
`components/esp_hal_mspi/esp32p4/include/hal/{psram_ctrlr_ll,mspi_ll}.h`,
Copyright 2019–2026 Espressif Systems (Shanghai) CO LTD, Apache-2.0.
Mode register commands call resident mask-ROM SPI helpers using the published
`components/esp_rom/esp32p4/include/esp32p4/rom/opi_flash.h` ABI and eco0_4 linker addresses.
No SDK object files are linked. One-buffer DMA reload and the diagnostics are local.
The DSI bridge framebuffer setup in `src/hal/dsi.rs` was checked against v6.1
`components/esp_lcd/dsi/esp_lcd_panel_dpi.c` and its corresponding HAL/LL.

`src/hal/sdmmc.rs` adapts native SDMMC clock/reset, command and internal IDMAC
register sequences from ESP-IDF v6.1 (same commit above), Apache-2.0:
`components/esp_hal_sd/esp32p4/include/hal/sdmmc_ll.h`,
`components/esp_driver_sdmmc/src/sd_host_sdmmc.c`,
`components/esp_hal_pmu/esp32p4/include/hal/ldo_ll.h` and hw_ver1
SDMMC/HP_SYS_CLKRST/LP_CLKRST/PMU register definitions,
Copyright 2023–2026 Espressif Systems (Shanghai) CO LTD.
The standalone SDHC protocol, bounded polling/cache synchronization and
read/write/restore diagnostic commands are local Rust code. No SDK code is linked.

SD SCR/SD Status/CMD6 field layouts were checked against ESP-IDF v6.1
`components/sdmmc/include/sd_protocol_defs.h` and `components/sdmmc/sdmmc_sd.c`.

`src/hal/i2s.rs` adapts I2S clock/format and AHB GDMA register sequences from
ESP-IDF v6.1 (same commit above), Apache-2.0, `esp_hal_i2s` and `esp_hal_dma`
P4 LL plus hw_ver1 register definitions. `src/devices/es8311.rs` adapts the
DAC register initialization and volume scale from Espressif `esp_codec_dev`,
Copyright 2023 Espressif Systems (Shanghai) CO LTD, Apache-2.0:
[es8311.c](https://github.com/espressif/esp-adf/blob/68ce88ad4dcf2cadc3f2d45635de5d4305e51498/components/esp_codec_dev/device/es8311/es8311.c).
The fixed buffer transport, test tone, bounded polling and PA cleanup are local.
No ADF/IDF runtime or C objects are linked.

Cache bootstrap and memory layout checks in src/main.rs / src/hal/memory.rs
reference ESP-IDF v6.1 hw_ver1 register definitions, linker memory layout and
components/esp_rom/patches/esp_rom_cache_writeback_esp32p4_esp32s31.c
(double writeback and SYNC_DONE), Copyright 2026 Espressif Systems (Shanghai)
CO LTD, Apache-2.0. Standalone region arenas and benchmark loops are local Rust.

`src/media/loeffler.rs` is a modified Rust adaptation of the 8-bit 8x8
`_jpeg_idct_islow` in libjpeg-turbo `src/jidctint.c`, pinned commit
`29e4d9b0178f08158fc72c6068f086c5ba27b772`:
https://github.com/libjpeg-turbo/libjpeg-turbo/blob/29e4d9b0178f08158fc72c6068f086c5ba27b772/src/jidctint.c
Original copyright 1991–1998 Thomas G. Lane, modifications 2002–2018 Guido
Vollbeding, libjpeg-turbo modifications 2015, 2020, 2022, 2026 D. R. Commander.
Rust modifications: 2026 ESP32-P4 project contributors.
Changes: already-dequantized i32 input, wide integer products/sums, explicit
saturating output instead of IJG's masked range-limit table, standalone Rust
without libjpeg data structures/runtime. Zero-row/column shortcuts retained.
The local matrix reference and coefficient/host diagnostics remain separate.

This software is based in part on the work of the Independent JPEG Group.
The applicable IJG conditions and copyright/no-warranty notice are included
unaltered in [libjpeg-turbo-README.ijg.txt](libjpeg-turbo-README.ijg.txt), with the
upstream license overview in [libjpeg-turbo-LICENSE.md](libjpeg-turbo-LICENSE.md).
No libjpeg Cargo/library dependency is linked. The host-only validation script
extracts pinned C into ignored target/, preserving its header and applying the
same documented input/range adaptations for an independent UBSan comparison.
