# ESP32-P4 rev 1.3 — свой минимальный runtime

Rust `no_std` / `no_main`, без Cargo-зависимостей, ESP-IDF, RTOS и готового HAL.
Target: `riscv32imafc-unknown-none-elf`, стабильный Rust.

Первый запуск дисплея, DSI и тестовые полосы: [docs/display.md](docs/display.md).

Возможности JD9165, документация панели и план графического тракта: [docs/jd9165.md](docs/jd9165.md).

Публичный `Display`, проверенные функции и команды USB-диагностики: [docs/display-api.md](docs/display-api.md).

Read-only RTC и результат проверки времени: [docs/rtc.md](docs/rtc.md).

Драйвер touch, события и карта памяти: [docs/gt911.md](docs/gt911.md).

I²C1 и обнаруженные устройства: [docs/i2c.md](docs/i2c.md).

Драйверы времени, GPIO, USB-консоли и watchdog, API и результаты аппаратной проверки: [docs/hal.md](docs/hal.md).

Подробно о режимах загрузки, DTR/RTS, причинах сбросов, watchdog и восстановлении: [docs/boot.md](docs/boot.md).

Путь запуска: **ROM → наш `_start` → Rust**. Образ по адресу flash `0x2000` заменяет второй загрузчик. Структура описания приложения и таблица разделов не используются.

```bash
export PATH="$HOME/.cargo/bin:$PATH"
./scripts/image.sh                  # ELF и target/firmware.bin
./scripts/flash.sh /dev/ttyACM0      # запись образа по 0x2000
python scripts/monitor.py /dev/ttyACM0
```

Необходимы Rust с target из `rust-toolchain.toml` и `espflash` (4.7.0). Для доступа к порту пользователь должен входить в `uucp`; после добавления группы выполните `newgrp uucp` в терминале.

`_start` в TCM включает clock SRAM, выключает machine interrupts, устанавливает стек, обнуляет BSS и вызывает Rust-код в SRAM. Код/константы занимают область с `0x4FF00000`, data/stack — с `0x4FF20000`, initial SP `0x4FF28000`. ROM загружает сегменты напрямую; MMU, кеши, PSRAM и второе ядро не используются.

HAL доступен через `esp32_p4::hal`; драйверы внешних устройств — через `esp32_p4::devices`. Текущая прошивка опрашивает GT911 на GPIO7/8, I²C1, 100 кГц, выдаёт `down / move / up` в USB и heartbeat со счётчиками кадров/ошибок. Дисплей запускается через собственный DSI HAL: аппаратный вертикальный BER-шаблон 1024×600 остаётся включённым постоянно, GPIO23 управляет подсветкой через LEDC PWM 20 кГц, 10 бит; яркость установлена на 5%, без стартового перебора уровней. Объект `devices::jd9165::Display` объединяет DSI, RESET и PWM; через USB доступны команды проверки функций панели (`python scripts/display-control.py '?'`). Framebuffer, PSRAM и DMA ещё не используются. Протокол, API и ограничения описаны в [docs/gt911.md](docs/gt911.md).

Сборка `espflash save-image --ignore-app-descriptor` создаёт только контейнер образа Espressif: заголовок ROM, загружаемые сегменты, checksum и SHA-256. Загрузчик и код ESP-IDF в него не входят.

Изменены flash по `0x2000` и, во время предшествующих проб, начало прежнего приложения по `0x10000`. Для возврата ассистента используйте полный проверенный дамп и инструкцию в локальном `backups/README.md`.

Основания для карты памяти и адресов регистров:

- [ROM загружает образ с 0x2000 в RAM](https://docs.espressif.com/projects/esp-idf/en/v6.1/esp32p4/api-guides/startup.html).
- [Карта памяти загрузки, ветка ревизий ниже v3](https://github.com/espressif/esp-idf/blob/v6.1/components/bootloader/subproject/main/ld/esp32p4/bootloader.memory.ld.in).
- [Регистры LP_WDT hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/lp_wdt_reg.h).
- [Регистры USB Serial/JTAG hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/usb_serial_jtag_reg.h).

Проверка на плате: ROM `esp32p4-eco2-20240710`, `SPI_FAST_FLASH_BOOT`, загрузка и entry `0x30100000`; приложение выдаёт события касаний и heartbeat через USB. Логи текущих проверок хранятся в `target/`.

Монитор использует только стандартную библиотеку Python (Linux). Он освобождает DTR/RTS и переподключается после USB reset. Во время прошивки монитор нужно остановить. В этой сессии обычный монитор espflash оставлял плату в download mode; для проверки запуска используется `scripts/monitor.py`.

Текущий образ — **26160 байт**. Read-only RTC выводится с heartbeat; VLF/VDET установлены, календарь некорректен, время не менялось. На плате подтверждены GT911 1024×600, поток касаний и подтверждение кадров. Результаты предыдущих проверок I²C, таймера и GPIO сохранены в документации. Минимальный исходный образ 368 байт и особенности запуска описаны в [docs/boot.md](docs/boot.md).

Локальный справочник по регистрам: `~/esp-idf`, tag `v6.1`. SDK не устанавливается и в сборку не подключается.

Настройки Zed находятся в `.zed/settings.json`: rust-analyzer проверяет embedded lib/bin без `--all-targets` и без принудительного `cfg(test)`. Иначе проверка host-тестов для bare-metal target вызывает `E0463: can't find crate for test`. Тесты логики запускаются отдельно на компьютере:

```bash
cargo test --lib --target x86_64-unknown-linux-gnu
```
