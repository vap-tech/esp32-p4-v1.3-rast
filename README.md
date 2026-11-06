Текущее состояние и ограничения: [docs/status.md](docs/status.md).

# ESP32-P4 rev 1.3 — свой минимальный runtime

Rust `no_std` / `no_main`, без Cargo-зависимостей, ESP-IDF, RTOS и готового HAL.
Target: `riscv32imafc-unknown-none-elf`, стабильный Rust.

Первый запуск дисплея, DSI и тестовые полосы: [docs/display.md](docs/display.md).

Возможности JD9165, документация панели и план графического тракта: [docs/jd9165.md](docs/jd9165.md).

Публичный `Display`, проверенные функции и команды USB-диагностики: [docs/display-api.md](docs/display-api.md).

Предложения по памяти, рендеру и буферам USB Display / USB-камеры: [docs/render-memory.md](docs/render-memory.md).

Один framebuffer и прямой поток камеры в PSRAM: [docs/framebuffer.md](docs/framebuffer.md).

Первый кадр камеры через CSI, SRAM DMA и особенности L1-кэша: [docs/csi.md](docs/csi.md).

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

`_start` в SPM включает clock SRAM, запрещает interrupts, сохраняет dirty cache data и переводит L1/L2 в bypass. L2 capacity=0 освобождает все 768 KiB L2MEM под SRAM. Код/константы — с `0x4FF00000`, data/BSS — с `0x4FF20000`; отдельная арена 496 KiB — с `0x4FF40000`, runtime stack 16 KiB — `0x4FFBC000…0x4FFC0000`. ROM-область сохраняется. Верхние 512 KiB проверяются до переключения стека. PSRAM 32 MiB отображается по команде и используется только uncached; первые 4 MiB отделены для дисплея/снимка, остальные 28 MiB — большая арена. Подробности, команды и измерения: [docs/memory.md](docs/memory.md).

HAL доступен через `esp32_p4::hal`; драйверы внешних устройств — через `esp32_p4::devices`. Текущая прошивка опрашивает GT911 на GPIO7/8, I²C1, 100 кГц, выдаёт `down / move / up` в USB и heartbeat со счётчиками кадров/ошибок. Дисплей запускается через собственный DSI HAL: аппаратные вертикальные цветные полосы 1024×600 остаётся включённым постоянно, GPIO23 управляет подсветкой через LEDC PWM 20 кГц, 10 бит; яркость установлена на 25%, без стартового перебора уровней. Объект `devices::jd9165::Display` объединяет DSI, RESET и PWM; через USB доступны команды проверки функций панели (`python scripts/display-control.py '?'`). По команде `F` включается один RGB565 framebuffer 1024×600 в PSRAM; `V` направляет камеру через ISP прямо в него. Подробности: [docs/framebuffer.md](docs/framebuffer.md). CSI принимает одиночный RAW10 crop 64×32 в SRAM через DW-GDMA; команды `a/A/o` и ограничения — в [docs/csi.md](docs/csi.md). Протокол, API и ограничения описаны в [docs/gt911.md](docs/gt911.md).

Сборка `espflash save-image --ignore-app-descriptor` создаёт только контейнер образа Espressif: заголовок ROM, загружаемые сегменты, checksum и SHA-256. Загрузчик и код ESP-IDF в него не входят.

Изменены flash по `0x2000` и, во время предшествующих проб, начало прежнего приложения по `0x10000`. Для возврата ассистента используйте полный проверенный дамп и инструкцию в локальном `backups/README.md`.

Основания для карты памяти и адресов регистров:

- [ROM загружает образ с 0x2000 в RAM](https://docs.espressif.com/projects/esp-idf/en/v6.1/esp32p4/api-guides/startup.html).
- [Карта памяти загрузки, ветка ревизий ниже v3](https://github.com/espressif/esp-idf/blob/v6.1/components/bootloader/subproject/main/ld/esp32p4/bootloader.memory.ld.in).
- [Регистры LP_WDT hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/lp_wdt_reg.h).
- [Регистры USB Serial/JTAG hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/usb_serial_jtag_reg.h).

Проверка на плате: ROM `esp32p4-eco2-20240710`, `SPI_FAST_FLASH_BOOT`, загрузка и entry `0x30100000`; приложение выдаёт события касаний и heartbeat через USB. Логи текущих проверок хранятся в `target/`.

Монитор использует только стандартную библиотеку Python (Linux). Он освобождает DTR/RTS и переподключается после USB reset. Во время прошивки монитор нужно остановить. В этой сессии обычный монитор espflash оставлял плату в download mode; для проверки запуска используется `scripts/monitor.py`.

Текущий образ — **141936 байт**. Read-only RTC выводится с heartbeat; VLF/VDET установлены, календарь некорректен, время не менялось. На плате подтверждены GT911 1024×600, поток касаний и подтверждение кадров. Результаты предыдущих проверок I²C, таймера и GPIO сохранены в документации. Минимальный исходный образ 368 байт и особенности запуска описаны в [docs/boot.md](docs/boot.md).

Локальный справочник по регистрам: `~/esp-idf`, tag `v6.1`. SDK не устанавливается и в сборку не подключается.

Настройки Zed находятся в `.zed/settings.json`: rust-analyzer проверяет embedded lib/bin без `--all-targets` и без принудительного `cfg(test)`. Иначе проверка host-тестов для bare-metal target вызывает `E0463: can't find crate for test`. Тесты логики запускаются отдельно на компьютере:

```bash
cargo test --lib --target x86_64-unknown-linux-gnu
```

Native microSD SDMMC slot 0, SDIO PLL200, 4-bit/25 MHz и High Speed 50 MHz,
многоблочные CMD18/CMD25 до 8 KiB (чтение 8 GB карты около 7.9 MiB/s): чтение/запись секторов и результаты
тестов на ноутбуке/плате — [docs/sdmmc.md](docs/sdmmc.md). Собственный FAT32 с каталогами, open/read/seek, create_new, двумя кешами и
пакетным чтением файлов: [docs/filesystem.md](docs/filesystem.md). Команда Z
проверяет реальные файлы на карте; команда ! создаёт тестовый CONFIG.TXT и
проверяет его после повторного mount. Изменение существующих файлов и длинные
имена пока не реализованы.

Аудио bring-up: I²S1 TX → ES8311 → NS4150, AHB DMA, тестовый тон командой p.
PA_CTRL=GPIO11, слышимый тон подтверждён пользователем; цифровой loopback
через GPIO48 тоже проверен. WAV с SD проверен на четырёхблочном DMA-кольце:
60 секунд без DMA errors, underrun останавливает канал и позволяет новый запуск.
Аналоговое качество WAV в этом тесте ещё требует подтверждения слушателем. Запись микрофона не реализована.
[docs/audio.md](docs/audio.md).

JPEG и WAV с карты включаются только командами, без автозапуска:

```sh
python3 scripts/display-control.py --image HOME.JPG
python3 scripts/display-control.py --play SATIE.WAV
python3 scripts/display-control.py --stop
```

JPEG 1024×600 с SD проверен на плате. Форматы, API, результаты и точка
остановки аудиоэксперимента: [docs/media.md](docs/media.md).

Проверка стоимости JPEG вывода без переключения кешей:
`python3 scripts/display-control.py --image HOME.JPG --jpeg-bench HOME.JPG`.
Проходы A–B–B–A сравнивают volatile u16 framebuffer writes и checksum-only.
Методика и результаты — [docs/media.md](docs/media.md).

Профиль JPEG по стадиям с контрольными проходами:
`python3 scripts/display-control.py --image HOME.JPG --jpeg-profile HOME.JPG`.
На текущем режиме без кешей IDCT занимает около 4.44 s / 59% wall time;
инструментирование добавляет около 31 ms / 0.42%. [Подробности](docs/media.md).

Read-only аудит CPU clock: `python3 scripts/display-control.py --clock`.
В текущем uncached режиме и повторении прежней cached версии подтверждены
около 160 MHz, CPLL / 2; отключение кешей частоту CPU не меняет.
IDCT integer i32/i64, без floating-point и runtime sin/cos. [Аудит](docs/media.md).

Отдельный Loeffler IDCT и сравнение с сохранённой матрицей:
`python3 scripts/display-control.py --image HOME.JPG --jpeg-idct-bench HOME.JPG`.
При 160 MHz без кешей полный decode **7.355 → 4.845 s**, IDCT **4.246 → 1.738 s**.
Матрица остаётся default; новый вариант выбирается явно. [Проверки и API](docs/idct.md).
This software is based in part on the work of the Independent JPEG Group.

L1 I-cache при нулевом L2 проверен на плате: `--icache on` возвращает примерно
двукратную скорость JPEG без потери SRAM, D-cache остаётся bypass. После reset
кеши выключены. [Методика, счётчики и ограничения](docs/icache.md).

Контроль L2=128KiB и L1 D-cache выполнен: для SRAM JPEG L2 не используется,
а оба L1 при L2=0 дают **2.628 s Matrix / 1.696 s Loeffler** с полной SRAM-ареной.
`--dcache on/off` — экспериментальный режим для JPEG/SD/LCD; default boot не менялся.
[Карта 128KiB L2, проверка DMA и результаты](docs/cache-matrix.md).

DMA validation: dirty SD RX, repeated LCD updates, four-block I2S streaming,
underrun/restart and mixed PCM + SD JPEG + LCD passed on rev1.3 with I+D ON.
`--play SATIE.WAV` now streams through a four-block ring; `--mixed-stress` uses
generated PCM with the speaker disabled. [Coverage and commands](docs/dma.md).

Real WAV/JPEG lab: one SD/FAT32 volume, independent readonly cursors, continuous
GPIO9 RX sample comparison and ring latency bounds. Both I-only and I+D passed
minute runs; the detector catches an injected repeated frame without DMA error.
Default DAC volume is now **-21 dB** (+3 dB). [Results and build](docs/wav-lab.md).
