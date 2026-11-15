# Состав прошивки и восстановление опциональных подсистем

Default image теперь содержит рабочие Ethernet/HTTP/SD/FAT, FileTable, дисплей,
часы/RTC/будильник, тач, воспроизведение и запись WAV, камеру и форматирование SD.
В default также включён `lp-timer`: автономный LP counter с периодом1 с
([lp-timer.md](lp-timer.md)); старый `lp-lab` заменяет этот сервис.
Также включён `hp-worker`: постоянный HP1 с отдельным стеком/очередями;
без заданий спит, вычислительная тестовая нагрузка выключена. См. [hp-worker.md](hp-worker.md).
Лабораторные команды, ESP-Hosted Wi-Fi и программный JPEG не включены по умолчанию.
Код и предыдущие исследования сохранены; зависимости C/ESP-IDF runtime не добавлены.

В default включён также `photoframe`: аппаратные JPEG/SRM и управление показом
из веба. Лабораторные команды не включаются вместе с ним автоматически.
Ограничения вывода и исходных JPEG: [photoframe.md](photoframe.md).

## Features

| Feature | Что добавляет |
|---|---|
| `photoframe` | Включён по умолчанию: веб-фоторамка, hardware-jpeg и hardware-ppa; без BLEND |
| `hardware-jpeg` / `hardware-ppa` | HAL аппаратуры без лабораторных команд |
| `lp-timer` | Включён по умолчанию: LP TIMER target1, собственный LP код и периодический сон/reset-entry |
| `ethernet-pie` | Опциональные128-bit копии Ethernet SRAM frames на HP0, без смены DMA/cache policy; [A/B/A](ethernet-pie.md) |
| `pie-lab` | Опциональные PIE2.1 SIMD/HWLOOP тесты на HP1; включает hp-worker, не добавляет C/runtime. См. [pie.md](pie.md) |
| `hp-worker` | Включён по умолчанию: фиксированный HP1 worker, IRQ18, pause/resume и защита cache maintenance |
| `multicore-lab` | Одноразовая HP1 boot probe; заменяет постоянный worker |
| `multicore-queue-lab` / `multicore-cache-lab` | Boot queue/IRQ probes, без кешей / с I0+I1+D ON; заменяют постоянный worker |
| `lp-lab` | LP SRAM code и три цикла сна/HP wake до обычного runtime |
| `diagnostics` | SRAM/PSRAM/cache, SD/DMA, IRQ, arbitration, RGB/chart/BIST, tone и другие лабораторные команды |
| `wifi` | C6 ESP-Hosted, SDIO function API, RPC, scan/connect, Wi-Fi web; прежняя конфигурация сети |
| `software-jpeg` | Программный JPEG decoder, IDCT implementations, команда display image |
| `audio-lab` | Длительный WAV+JPEG/digital verification; включает diagnostics и software-jpeg |
| `sdio-reg-cmd52` | Прежний контрольный способ чтения C6 registers; включает wifi |
| `l2-128` | Историческая экспериментальная раскладка SRAM/L2; включает diagnostics |

`pie-lab` требует runtime worker; сочетания `multicore-lab`/`l2-128`,
отключающие его, также отключают доступ к PIE-командам.

JPEG benchmarks и mixed JPEG+PCM stress требуют одновременно diagnostics и
software-jpeg. Wi-Fi raw transport test требует wifi и diagnostics. Просто
`--features diagnostics` не включает радио или программный JPEG.

RAM speed test в вебе, memory/clock status и SD capabilities остаются в core:
это текущие средства проверки работающей системы. `sd list` старой консоли
выполнял также read/rewind/hash probe до1MiB и теперь относится к diagnostics;
обычный список файлов доступен через веб. SD config-create также лабораторный.
Format в default проверяет геометрию/пустой root, но не создаёт пробный CONFIG.TXT;
этот дополнительный тест сохранён для diagnostics.

## Сборка

```sh
# Текущий обычный образ, без Wi-Fi/JPEG/labs:
./scripts/image.sh

# Только HP0, без runtime HP1/LP, для воспроизведения прежних замеров:
./scripts/image.sh --no-default-features

# Эксперименты SD/памяти/дисплея, без радио и JPEG:
./scripts/image.sh --features diagnostics

# Только возврат Wi-Fi; JPEG и лабораторные тесты остаются выключены:
./scripts/image.sh --features wifi

# Старые JPEG-эксперименты:
./scripts/image.sh --features diagnostics,software-jpeg

# Сохранённая полная конфигурация, включая WAV/JPEG lab:
./scripts/image.sh --features diagnostics,wifi,software-jpeg,audio-lab

# После выбранной сборки — обычная прошивка P4:
espflash write-bin --port /dev/ttyACM0 0x2000 target/firmware.bin
```

Обычный `--features diagnostics` сохраняет HP1. Для старых single-core JPEG/cache
замеров добавляйте `--no-default-features`. Изменение кешей при живом worker
возвращает WorkerActive: требуется подтверждённая `system worker pause` и
соблюдение требований DMA. `l2-128` автоматически исключает runtime worker.

Не использовать `--all-features` как синоним полной рабочей firmware: l2-128
меняет карту памяти и может не вмещать полный runtime. Linker guards сохранены.
Каждый image должен собираться вместе с его bootstrap/manifest, не смешивать
payload из разных конфигураций. Сам C6 этими командами не прошивается.

## Как вернуть Wi-Fi после проверки антенны

Собрать/прошить P4 с wifi, затем:

```sh
python3 scripts/display-control.py --wifi-web 3600
# Для raw SDIO сначала сборка с wifi,diagnostics:
python3 scripts/display-control.py --wifi-raw rx --raw-seconds 10
```

Работавшая C6 firmware3.0.9 / IDFv6.1 / RPCv2 сохранена на чипе. Её обновление
не требуется для возврата к этой версии host driver. Профиль — src/wifi_profile.rs;
пароль не удалён и не выводится в диагностике. GPIO54 reset active-low,
GPIO14..19 SDIO slot1; SD memory slot0 сериализуется тем же controller owner.
В default эти модули и C6 RX/TX buffers не компилируются; это отключение host
software, не обещание аппаратного снятия питания с C6.

Все результаты, включая−75dBm scan, raw SDIO throughput, TCP capture с SACK
и window-update/fast-retransmit находку, остаются в [wifi.md](wifi.md).
Оригинальная причина потерь радио/SDIO и исправление TCP recovery ещё не закрыты.
При восстановлении wifi web нужно снова проверить общую SDMMC arbitration;
общий backend двух сетевых транспортов пока не рефакторили.

## JPEG и будущий аппаратный путь

HTTP продолжает хранить, загружать и отдавать .jpg; браузерный preview работает.
Выключено декодирование JPEG внутри P4 и команда его вывода на экран.
Framebuffers/DSI/CSI, шрифты часов и аппаратные display patterns сохраняются.
Аппаратный JPEG decoder, PPA/DMA2D и иной graphics pipeline этим feature не
реализованы — ими займёмся отдельно, независимо от software-jpeg.

Matrix default старого software JPEG сохранён без изменений. Причина описана
в [memory-audit.md](memory-audit.md): ранее Loeffler выбирался явно в экспериментах.
Исходники matrix/Loeffler, host fixtures, профили и результаты не удалены.

## Команды и контроль состава

Третья колонка commands.tsv/command-routes.tsv задаёт обязательные features
(запятая означает AND). build.rs создаёт активные двухколоночные таблицы в
OUT_DIR. Firmware dispatch и help используют именно их. Не собранная команда
не принимается, а не просто исчезает из списка.

`help` печатает BUILD diagnostics/wifi/software-jpeg/audio-lab. Host helper читает
этот список и сразу сообщает требуемый feature для отсутствующей команды,
вместо ожидания ответа выключенного handler. Полная карта команд остаётся
в исходных TSV и поддерживает обе конфигурации.

## Память и проверки

До разделения:payload371732B,work82368B,scoped DMA116KiB,fixed reserve36KiB.
После:payload около261192B (255.07KiB),work около189KiB,
scoped DMA136KiB,fixed reserve16KiB. Точные величины конкретного ELF:
`target/build-flavors/report.json`, serial memory status. Firmware с Flash asset
и padding по-прежнему около1MiB; размер flash image не равен SRAM payload.

Без wifi fixed SD/I2S buffers занимают16KiB, поэтому20KiB прежнего резерва
перешли в нижний DMA pool. С feature wifi автоматически возвращается
116KiB pool /36KiB fixed. Boot stack, ROM reserve и64KiB runtime stack не менялись.

Проверены: default release; diagnostics-only release; optional release с
wifi/software-jpeg/audio-lab/diagnostics; Clippy default, wifi-only, JPEG-only
и all-features. Host:81 default,91 optional (+2 ignored JPEG reference tests).
Тесты проверяют также соответствие advertised commands features сборки.
На плате core: memory layout check, два отключённых subsystem command и IRQ-test
отклоняются helper немедленно с feature hint; Ethernet RAM, SD PUT/GET, reconnect,
3 clients и чередование файлов прошли. Два PUT завершились при
удерживаемом GET, четвёртый abort удалён; SHA/root unchanged. Не форматировали SD.
Проверка features выполнялась без включения Wi-Fi на плате.

Диагностический файловый SD benchmark и append-маршрут: [storage-write-profile.md](storage-write-profile.md).

`sd-wide-bench` — отдельный экспериментальный образ с 32-КиБ SD-запросами и дополнительной статической SRAM; описание и ограничения: [sd-write-batching.md](sd-write-batching.md).

Обновление sd-wide-bench: теперь четыре пути SRAM/PSRAM × bounce/direct, все по 32 КиБ; [результаты и контракт DMA](sd-psram-dma.md).

Прямой TX и 32-КиБ FileTable теперь доступны в default для проводного PSRAM upload; большие SRAM statics остаются диагностическими. [Описание](web-psram-upload.md).

`jpeg-lab` — аппаратный JPEG + 2D-DMA, отдельная экспериментальная команда
`display jpeg hardware`. Не включает software-jpeg. Ограничения и workaround
AXI RID counter описаны в [jpeg.md](jpeg.md). Команда остаётся opt-in,
а HAL также используется штатной фоторамкой.

`ppa-lab` — offscreen PPA RGB565 FILL/SRM; команды `display ppa test`,
`display ppa srm`, `display ppa srm-large`, `display ppa srm-stress`.
Не включает JPEG и не меняет scanout; [контракт и замеры](ppa.md).

SRM: [геометрия, качество и измерения](ppa-srm.md).

`ppa-lab` также содержит экспериментальный BLEND и отдельные `blend*-probe`
команды; RID error остаётся открытым. [Контракт](ppa-blend.md).
