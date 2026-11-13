Текущее состояние и ограничения: [docs/status.md](docs/status.md).

# ESP32-P4 rev 1.3 — свой минимальный runtime

Rust `no_std` / `no_main`, собственный HAL без ESP-IDF и RTOS.
Default: Ethernet и рабочие устройства; labs/Wi-Fi/software JPEG — отдельные features.
[Состав сборок и команды восстановления](docs/build-flavors.md).
TCP/IP — smoltcp; веб-ФС — embedded-fatfs. Всё без `std`/heap; HTTP и HAL собственные.
Аудит размера кода и кандидаты на уборку: [docs/memory-audit.md](docs/memory-audit.md).
Несколько файлов и append: [docs/file-handles.md](docs/file-handles.md).
Асинхронное SD/ФС и границы миграции: [docs/async-storage.md](docs/async-storage.md).
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

`_start` в SPM сохраняет dirty ROM cache data, переводит L1/L2 в bypass и
настраивает L2MEM как SRAM. Затем собственный bootstrap читает runtime payload
из Flash, проверяет CRC32 и только после этого запускает Rust из верхней SRAM.
Код/константы/статика начинаются с `0x4FF40000`; рабочая арена динамически идёт
за ними. Нижние **152 KiB** разделены на арену **116 KiB** и fixed DMA **36 KiB**;
runtime stack — **64 KiB**,
область ROM сохранена. Полного destructive SRAM test при старте больше нет.
PSRAM остаётся 32 MiB uncached: 4 MiB display/screenshot + 28 MiB bulk arena.
[Актуальная загрузка и карта](docs/stage2-boot.md), [история экспериментов](docs/memory.md).

`image.sh` сначала создаёт `target/ram-image.bin` через espflash, затем наш
`pack-image.py` собирает окончательный **target/firmware.bin** с bootstrap,
manifest и payload. Промежуточный ram-image напрямую не прошивать.
Команды Ethernet и форматирования теперь входят в обычный образ;
размерные features `eth-lab` / `sd-format` удалены. `audio-lab` только добавляет
RX capture tests, не отключает штатные команды.

HAL доступен через `esp32_p4::hal`; драйверы внешних устройств — через `esp32_p4::devices`. Текущая прошивка опрашивает GT911 на GPIO7/8, I²C1, 100 кГц, выдаёт `down / move / up` в USB и heartbeat со счётчиками кадров/ошибок. Дисплей запускается через собственный DSI HAL: при загрузке включается салатовая заливка RGB565 framebuffer 1024×600 в PSRAM, GPIO23 управляет подсветкой через LEDC PWM 20 кГц, 10 бит; яркость установлена на 25%, без стартового перебора уровней. Объект `devices::jd9165::Display` объединяет DSI, RESET и PWM; через USB доступны команды проверки функций панели (`python scripts/display-control.py help`). Один RGB565 framebuffer 1024×600 в PSRAM включается автоматически; `camera start` направляет камеру через ISP прямо в него. Подробности: [docs/framebuffer.md](docs/framebuffer.md). CSI принимает одиночный RAW10 crop 64×32 в SRAM через DW-GDMA; именованные команды `camera info` / `camera capture-test` / `camera scene` и ограничения — в [docs/csi.md](docs/csi.md). Протокол, API и ограничения описаны в [docs/gt911.md](docs/gt911.md).

Промежуточный `espflash save-image --ignore-app-descriptor` создаёт контейнер Espressif: заголовок ROM, загружаемые сегменты, checksum и SHA-256. Сторонний загрузчик и код ESP-IDF не добавляются.

Изменены flash по `0x2000` и, во время предшествующих проб, начало прежнего приложения по `0x10000`. Для возврата ассистента используйте полный проверенный дамп и инструкцию в локальном `backups/README.md`.

Основания для карты памяти и адресов регистров:

- [ROM загружает образ с 0x2000 в RAM](https://docs.espressif.com/projects/esp-idf/en/v6.1/esp32p4/api-guides/startup.html).
- [Карта памяти загрузки, ветка ревизий ниже v3](https://github.com/espressif/esp-idf/blob/v6.1/components/bootloader/subproject/main/ld/esp32p4/bootloader.memory.ld.in).
- [Регистры LP_WDT hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/lp_wdt_reg.h).
- [Регистры USB Serial/JTAG hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/usb_serial_jtag_reg.h).

Проверка на плате: ROM `esp32p4-eco2-20240710`, `SPI_FAST_FLASH_BOOT`, загрузка и entry `0x30100000`; приложение выдаёт события касаний и heartbeat через USB. Логи текущих проверок хранятся в `target/`.

Монитор использует только стандартную библиотеку Python (Linux). Он освобождает DTR/RTS и переподключается после USB reset. Во время прошивки монитор нужно остановить. В этой сессии обычный монитор espflash оставлял плату в download mode; для проверки запуска используется `scripts/monitor.py`.

Текущий stage-2 образ с HTTP и Flash-assets — около **1 MiB** с padding;
загружаемый runtime — около **189 KiB**. Точные размеры выводит `scripts/image.sh`.
Исторические образы до HTTP занимали 420656 B, а до stage-2 — 141936 B. Read-only RTC выводится с heartbeat; VLF/VDET установлены, календарь некорректен, время не менялось. На плате подтверждены GT911 1024×600, поток касаний и подтверждение кадров. Результаты предыдущих проверок I²C, таймера и GPIO сохранены в документации. Минимальный исходный образ 368 байт и особенности запуска описаны в [docs/boot.md](docs/boot.md).

Локальный справочник по регистрам: `~/esp-idf`, tag `v6.1`. SDK не устанавливается и в сборку не подключается.

Настройки Zed находятся в `.zed/settings.json`: rust-analyzer проверяет embedded lib/bin без `--all-targets` и без принудительного `cfg(test)`. Иначе проверка host-тестов для bare-metal target вызывает `E0463: can't find crate for test`. Тесты логики запускаются отдельно на компьютере:

```bash
cargo test --lib --target x86_64-unknown-linux-gnu
```

Native microSD SDMMC slot 0, SDIO PLL200, 4-bit/25 MHz и High Speed 50 MHz,
многоблочные CMD18/CMD25 до 8 KiB (чтение 8 GB карты около 7.9 MiB/s): чтение/запись секторов и результаты
тестов на ноутбуке/плате — [docs/sdmmc.md](docs/sdmmc.md). Собственный FAT32 с каталогами, open/read/seek, create_new, потоковым create_stream/write/finish/abort, двумя кешами и
пакетным чтением файлов: [docs/filesystem.md](docs/filesystem.md). Команда Z
проверяет реальные файлы на карте; команда ! создаёт тестовый CONFIG.TXT и
проверяет его после повторного mount. Изменение существующих файлов и длинные
имена пока не реализованы.

Аудио bring-up: I²S1 TX → ES8311 → NS4150, AHB DMA, тестовый тон командой p.
PA_CTRL=GPIO11, слышимый тон подтверждён пользователем; цифровой loopback
через GPIO48 тоже проверен. WAV с SD проверен на четырёхблочном DMA-кольце:
60 секунд без DMA errors, underrun останавливает канал и позволяет новый запуск.
Аналоговое качество WAV в этом тесте ещё требует подтверждения слушателем. Запись микрофона поддерживается; см. ниже и docs/recorder.md.
[docs/audio.md](docs/audio.md).

Для JPEG нужна сборка с `software-jpeg`, для JPEG benchmarks — ещё `diagnostics`.
WAV остаётся в default. Примеры команд:

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

Веб-файлы Flashbox: `python3 scripts/display-control.py --web-run 3600`.
Адрес печатается в `WEB READY`: страница из Flash, список, чередующиеся GET/PUT
на SD через таблицу файлов. Без перезаписи существующих файлов, поддержаны Unicode LFN. Свободно
около 189 KiB work SRAM; отдельно DMA arena136 KiB и fixed DMA16 KiB. [API и проверки](docs/web.md).

Консоль принимает только полные строки `:command [args]` + Enter; одиночные буквы больше не выполняются. Общий список — [docs/console.md](docs/console.md), в скрипте: `python3 scripts/display-control.py help`.

Запись микрофона: `python3 scripts/display-control.py --record REC0004.WAV --record-seconds 30`. Новый WAV на SD, STOP/ESC завершает файл; [проверки и ограничения](docs/recorder.md).

Для следующих Wi-Fi-команд нужна сборка с feature `wifi`; код и настройки сохранены.
Wi-Fi сопроцессор C6: `python3 scripts/display-control.py --wifi-probe`
читает версию, `--wifi-scan` сканирует до8 AP без подключения. Проверено с
существующим ESP-Hosted3.0.9/IDFv6.1, SDIO slot1 4bit/25MHz. Подробности и
ограничения совместной работы с SD: [docs/wifi.md](docs/wifi.md).

Подключение Wi-Fi в RAM: `python3 scripts/display-control.py --wifi-connect YOUR_SSID --wifi-seconds 300`;
пароль вводится без echo. Работают свой DHCP/ARP/ping; HTTP и SD одновременно
с Wi-Fi пока не обслуживаются. По завершении окна радио отключается.

Встроенная сеть владельца: `python3 scripts/display-control.py --wifi-default --wifi-seconds 300`.
Профиль включён во Flash P4 с явного разрешения, пароль больше не запрашивается.
Автоподключение при загрузке не включено.

Веб-файлы по Wi-Fi: `python3 scripts/display-control.py --wifi-web 3600`
(встроенный профиль, URL из WEB READY; [подробности](docs/wifi.md)).
