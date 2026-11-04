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
