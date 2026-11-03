# ESP32-P4 rev 1.3 — свой минимальный runtime

Rust `no_std` / `no_main`, без Cargo-зависимостей, ESP-IDF, RTOS и готового HAL.
Target: `riscv32imafc-unknown-none-elf`, стабильный Rust.

I²C1, обнаруженные устройства и текущая карта памяти: [docs/i2c.md](docs/i2c.md).

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

`_start` выключает machine interrupts, устанавливает стек, обнуляет BSS и вызывает Rust. Используются только целочисленные инструкции; FPU, второе ядро, PSRAM, кеши и периферия платы пока не настраиваются. Код и константы ROM загружает в 8 КиБ HP TCM с адреса `0x30100000`; стек/BSS размещены отдельно в 16 КиБ SRAM с `0x4FF00000`. Копирование `.data` из flash в нашем runtime не требуется.

HAL доступен как библиотечная часть пакета (`esp32_p4::hal`), драйверы находятся в `src/hal/`. Текущая прошивка раз в 5 секунд сканирует I²C1 на GPIO7/8 (100 кГц), читает ID устройств и выполняет короткие проверки FIFO/NACK. GPIO23 оставлен low, подсветка выключена. Таймер и GPIO ранее отдельно проверены на плате; результаты сохранены в [docs/hal.md](docs/hal.md).

Сборка `espflash save-image --ignore-app-descriptor` создаёт только контейнер образа Espressif: заголовок ROM, загружаемые сегменты, checksum и SHA-256. Загрузчик и код ESP-IDF в него не входят.

Изменены flash по `0x2000` и, во время предшествующих проб, начало прежнего приложения по `0x10000`. Для возврата ассистента используйте полный проверенный дамп и инструкцию в локальном `backups/README.md`.

Основания для карты памяти и адресов регистров:

- [ROM загружает образ с 0x2000 в RAM](https://docs.espressif.com/projects/esp-idf/en/v6.1/esp32p4/api-guides/startup.html).
- [Карта памяти загрузки, ветка ревизий ниже v3](https://github.com/espressif/esp-idf/blob/v6.1/components/bootloader/subproject/main/ld/esp32p4/bootloader.memory.ld.in).
- [Регистры LP_WDT hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/lp_wdt_reg.h).
- [Регистры USB Serial/JTAG hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/usb_serial_jtag_reg.h).

Проверка на плате: ROM `esp32p4-eco2-20240710`, `SPI_FAST_FLASH_BOOT`, загрузка и entry `0x30100000`; приложение повторно выводит маркер через USB. Логи текущих проверок хранятся в `target/`.

Монитор использует только стандартную библиотеку Python (Linux). Он освобождает DTR/RTS и переподключается после USB reset. Во время прошивки монитор нужно остановить. В этой сессии обычный монитор espflash оставлял плату в download mode; для проверки запуска используется `scripts/monitor.py`.

Текущий образ — **8048 байт**. На плате подтверждены пять адресов I²C, ID ES8311/GT911/камеры, RX32, раздельные write/read с STOP и обработка NACK. Минимальный исходный образ 368 байт и особенности запуска описаны в [docs/boot.md](docs/boot.md).

Локальный справочник по регистрам: `~/esp-idf`, tag `v6.1`. SDK не устанавливается и в сборку не подключается.
