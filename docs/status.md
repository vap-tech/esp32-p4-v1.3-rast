# Состояние проекта — 2026-11-06

## Live WAV + JPEG + LCD with digital verification

One SD/FAT32 volume now supports independent readonly SharedFile cursors.
Minute runs passed with I ON / D OFF and with I+D ON: 23/34 JPEG frames,
965120/983552 consecutively verified stereo frames, zero sample mismatches and
TX errors. Observed min OWN count=3, max refill latency upper bound 4.893/3.649 ms.
Injected repeat detected at the exact frame; injected delay counted one underrun.
Stack peak 10812 B; scratch preallocated in SRAM. DAC -21 dB (+3 dB).
[Coverage, limits and diagnostic build](wav-lab.md).


## DMA validation after stable baseline

SD dirty-RX / guard soak passed with I+D ON; LCD passed repeated JPEG updates.
I2S now uses four 2048 B blocks, keeping owner check and auto writeback enabled.
SD WAV played for 60 s with LCD active; deliberate producer stall stops DMA,
and a subsequent playback starts normally. Mixed generated PCM + SD JPEG + LCD
passed 20 frames with intact guards. Generic typed DMA and CSI/ISP remain pending.
Current boot defaults remain all-bypass; runtime experiments are opt-in.
[Commands, actual coverage, memory reuse and logs](dma.md).


## Стабильная база памяти и JPEG

Кодовая база зафиксирована коммитом **9afb4d1**. Экспериментальные переключатели
включены в репозиторий, но не стали политикой запуска устройства:

- После reset: L1 I/D и L2 bypass, L2=0; 496 KiB арена SRAM, 16 KiB stack,
  32 MiB PSRAM через uncached alias (28 MiB отдельная арена).
- Проверенный runtime I ON / D OFF сохраняет правила остальных DMA-клиентов.
- I+D ON при L2=0 проверены для JPEG/SD/LCD: Loeffler **1.696 s**;
  универсальная DMA-когерентность и длительные нагрузочные тесты ещё не завершены.
- Обычный `--image` сохраняет матричный IDCT; Loeffler выбирается явно.
- При Loeffler + I+D доли IDCT и entropy примерно **31% и 30%**. Старые 59%
  относятся к матрице без кешей; их нельзя переносить на текущий быстрый режим.

[Текущая матрица режимов, профиль и план DMA](cache-matrix.md).
Разделы ниже сохраняют историю отдельных измерений и соответствующих им сборок.


## Работает

- Bare-metal Rust на ESP32-P4 rev1.3: собственный HAL, без Cargo-зависимостей,
  ESP-IDF runtime, RTOS и второго загрузчика. ROM запускает образ с flash 0x2000.
- I²C, касания GT911, read-only RTC, LEDC PWM подсветки 20 kHz.
- JD9165: DSI, аппаратные полосы/BER и проверенный BIST самой панели,
  диагностические DCS-команды. Стартовый режим — обычные вертикальные полосы.
- AP Memory PSRAM 32 MiB, доступ на 80 MHz, весь объём отображён и проверен.
  Эксперимент без кешей: L1/L2 bypass, L2 size=0; 496 KiB внутренняя арена,
  runtime stack 16 KiB, внешняя арена 28 MiB. [Память](memory.md).
- Один полный RGB565 framebuffer 1024×600 (1 228 800 B). LCD читает его через
  DMA channel 1; камера через CSI/ISP пишет непосредственно в него по channel 0.
  Нет полного промежуточного RAW/RGB-буфера камеры и копирования каждого кадра CPU.
- OV02C10, ID 56 02 43: одиночные RAW10-пробы 64×32 в SRAM и непрерывный
  RAW10 → demosaic GBRG → RGB565 crop 1024×600 → PSRAM → LCD.
- Ручные exposure/analogue gain/digital gain с чтением регистров обратно.
- Полный screenshot PNG 1024×600: остановка CSI DMA, копия в отдельную область
  PSRAM, возобновление потока, передача копии по USB. Передача всех пикселей
  проверена на эталонных RGB-полосах.

- Ethernet: IP101 family addr1 ID0243:0c54, RMII external 50MHz, 100/full;
  собственный MAC DMA, DHCP/ARP/ICMP проверены. Финальный образ выдержал 1000 ping
  и отдельный повтор со 100 MTU + 100 разных payload без потерь/DUP/RX errors.
  D-cache OFF/L2=0; только явный timed `eth-lab` run, не фоновая служба,
  TCP/HTTP/fileserver ещё нет. [Команды и ограничения](ethernet.md).
- SDXC 250.20 GB проверена на борде: 4-bit/50 MHz, read-only сравнения начала,
  середины и конца совпали. Собственный quick formatter (`sd-format` build)
  создал MBR/FAT32 за 19.075 s, 32 KiB clusters, две FAT, backup VBR/FSInfo;
  CONFIG.TXT создан и проверен после remount и аппаратного reset.
  [Команды и ограничения форматирования](filesystem.md#явное-форматирование-sd).
- Native SDMMC slot 0: SDHC 8.01 GB, SDIO PLL200, 4-bit/25 MHz и согласованный High Speed 50 MHz. Чтение и запись секторов
  через собственный IDMAC SDMMC с cache sync, проверены MBR/FAT32 WEBRADIO,
  повторное чтение и write/read/restore последнего сектора. Чтение около
  1.17 MiB/s при отдельном CMD17 на каждый сектор.
  SCR/SD Status/CMD6 CHECK объявили SD 3.0x, C10, 1/4-bit и High Speed
  до 50 MHz; configure_bus переключает объявленный High Speed и запускает
  50 MHz от отдельного SDIO PLL200. Чтение до/после и запись проверены. Отдельный опрос Y
  остаётся на 1-bit/400 kHz.
  Добавлена SDSC: CSD v1, CMD16 и байтовая адресация. На карте 998 MB
  FAT32 SK1_TFT проверено чтение 1-bit/400 kHz, 4-bit/25/50 MHz;
  повторные 1 MiB совпали, до 1.68 MiB/s. Она объявляет SD 2.00 и Class 2;
  запись на этой карте пока не проверялась.
  CMD18/CMD25 с auto CMD12 и цепочкой DMA до 8 KiB реализованы: чтение
  8 GB карты ускорилось с 1.17 до 7.91 MiB/s, все сравнения совпали.
  Запись/чтение/восстановление 9 секторов проверены; консоль m и #.
  [Результаты SDMMC и тест ноутбука](sdmmc.md).
- Собственный FAT32: MBR-разделы, каталоги, ASCII 8.3, open/read/seek,
  независимые кеши метаданных/краёв по 512 B, пакетное чтение до 8 KiB
  с объединением соседних кластеров. На плате прочитаны список радиостанций
  и LOGOS/DUMMY.565 (11552 B), хеши при разных размерах чтения совпали.
  Консоль Z. Добавлен синхронный create_new без перезаписи: FAT/mirrors,
  выделение кластеров, FSInfo/backup, dirty flags и публикация записи каталога
  после данных. На SD создан CONFIG.TXT 126 B, после remount и перезагрузки
  все байты совпали; команда !. Настройки из файла пока не применяются.
  [Файловый слой](filesystem.md).

- Аудио: I²S1 TX, AHB DMA TX0, ES8311 DAC и **NS4150 PA GPIO11**.
  Команда p: тихий тон 1 kHz / 16 kHz PCM16 на 2 s, mute и PA off после.
  Пользователь подтвердил приятный короткий тон после исправления PA20→PA11.
  Короткий RX / AHB DMA RX0 проверяет собственный PCM и цифровой DAC-референс
  на GPIO48. WAV playback с SD на четырёхблочном кольце прошёл 60 s без DMA errors;
  underrun/restart проверены, аналоговое качество WAV пока не подтверждено слушателем.
  Аналоговый микрофон не захватывается.
  [Аудио](audio.md).

## Текущие параметры

| Параметр | Значение |
| --- | --- |
| LCD DPI | 48 MHz, согласованные HTOTAL=1344 / HLINE=1925, расчётная развёртка около 56.33 Hz |
| DSI | 2 lanes × 550 Mbps |
| Подсветка по умолчанию | 25% |
| Exposure камеры | 1120 строк, VTS 1164 |
| Analogue / digital gain | 1x / 1x, также по умолчанию |
| Формат камеры | RAW10, crop 1024×600 из режима 1920×1080 |
| Preview после reset | Включается явно командами F V |
| Размер текущего образа | 134 848 B |

## Известные проблемы и незавершённое

- SDMMC/FAT32: потоковое создание готово (`create_stream/write/finish/abort`,
  512 B edge buffer), frontend serializes PUT and rejects unsupported names.
  TCP/HTTP PUT пока не подключён. Нужны изменение существующих файлов и append,
  удаление, расширение каталогов, длинные имена, несколько открытых файлов,
  полноценный файловый бенчмарк и обработка извлечения карты.
  File/FileWriter эксклюзивно заимствуют том; readonly SharedFile cursors уже
  реализованы. create_new — обёртка над stream writer.
  Журналирования/автоматического восстановления нет.


**Камера пока даёт очень тёмное изображение даже при ярком освещении комнаты.**
Люстра и лицо видны, но много теней, заметны шум и зелёно-голубой оттенок.
Нормальная цветопередача и пригодное качество камеры пока не достигнуты.
Нельзя считать это доказанным пределом или неисправностью сенсора: тракт
обработки и его калибровка ещё не доведены.

- Настроенных AE/AWB, gamma, BF denoise и калиброванной цветовой матрицы нет.
  Попытка добавить BF, баланс каналов через CCM и gamma ухудшила результат;
  **полностью отменена**, экспериментальный код и команды удалены, прежний
  preview перепрошит и проверен. Подбор коэффициентов не является калибровкой.
- Не завершены измерение уровня чёрного, проверка Bayer по цветовой мишени,
  калибровка цвета/экспозиции и проверка характеристик объектива/модуля.
- На 30 MHz DPI физический LCD искажал цвета RGB-таблицы. При 48 MHz пользователь
  подтвердил правильные красный/зелёный/синий и серую шкалу. Причина зависимости
  от частоты не установлена. Byte swap, инверсия и смена BER на обычные полосы
  проблему не устранили; программного swap/инверсии сейчас нет.
- В ранних совместных тестах камера+LCD возникал underrun; в последних коротких
  окнах на 48 MHz его не было. Долговременная стабильность под нагрузкой не
  подтверждена; автоматического восстановления потока после ошибок нет.
- Один framebuffer допускает tearing. Остановка для screenshot пока не
  синхронизирована с EOF: копия стабильна при передаче, но на движении может
  содержать границу двух соседних кадров.
- Нет полноценного масштабирования/1080p preview, измеренного FPS камеры,
  HDR, постоянного автоэкспонирования и автоматического баланса белого.
- PSRAM выше 80 MHz пока не проверена. Все 32 MiB доступны uncached.
- RTC с VLF/VDET и некорректным календарём; время не настраивалось.

**По решению пользователя работу над камерой откладываем.** Текущее состояние
сохранено; без нового запроса камеру и её настройки дальше не меняем.

Подробности: [framebuffer](framebuffer.md), [CSI](csi.md),
[варианты рендера и USB](render-memory.md). Файлы логов/PNG в target и старые
исходники в old остаются локальными и не входят в Git.

## Медиа с SD — историческая пауза до DMA-validation

JPEG baseline 1024×600 из HOME.JPG декодирован CPU в RGB565 framebuffer,
scanout active=true, guards=true; около 2.72 s вместе с инициализацией SD.
Автопоказа картинки и автозапуска музыки нет: только команды скрипта.
WAV PCM16 parser и кольцо AHB DMA из двух буферов добавлены, но при воспроизведении
SATIE.WAV возникает descriptor error до первого refill; пользователь звука не слышит.
На этом этапе доработка была остановлена по просьбе пользователя. После отдельного
разрешения работа возобновлена и кольцо исправлено: [актуальные проверки](dma.md).
Подробности прежнего состояния: [media.md](media.md).

## Эксперимент без кешей

В плату прошит образ с bypass L1/L2 и L2 capacity=0. Верхние 512 KiB SRAM
проверяются до использования; arena=496 KiB, stack=16 KiB (watermark 9392 B
после JPEG). PSRAM 32 MiB целиком проверена, отдельная большая арена 28 MiB.
При LCD scanout CPU scalar PSRAM read/write около 5.7/11.5 MiB/s, без scanout
8.5/18.7 MiB/s. Framebuffer hash сохранён, SD read-only regression прошла.
Аудио/камера в этом заходе не запускались. Все цифры и ограничения: [memory.md](memory.md).

## JPEG A/B без записи framebuffer

Команда `--jpeg-bench HOME.JPG` делает A–B–B–A с одинаковым decoder/checksum
и активным LCD: A=7.435144 s, B=7.260713 s, разность 174.431 ms / 2.35%.
SD I/O ~48 ms, hashes совпали с host reference; framebuffer не меняется в B.
Значит, основное время этого JPEG уходит не на PSRAM stores; профиль по стадиям
CPU пока не выполнялся. Кеши не переключались, GDMA не добавлялся.
Текущий watermark stack 10076/16384 B, guard=true. [Методика и результаты](media.md).

## Профиль JPEG по стадиям

Без смены кешей/алгоритма: entropy+dequant 1.436 s, IDCT **4.440 s**,
color 0.996 s, output+checksum 0.332 s, residual 0.356 s. Контроль без stage clock
7.530 s, profile 7.562 s, overhead 31 ms / 0.42%. Все hashes прежние, 2432 MCU,
14592 blocks, 614400 pixels. Hot functions и BASIS по ELF/disassembly в SRAM;
mcycle calibration ~160 MHz. Дополнительный MCU RGB scratch 512 B одинаков
в профиле и контроле, stack peak 10988/16384 B, guard=true.
34 host tests прошли. Кеши не переключались, алгоритм IDCT не оптимизировался.
[Команда --jpeg-profile и полная методика](media.md).

## CPU clock и IDCT arithmetic audit

Cached reference из 9288004 и текущий uncached runtime: mcycle ~160 MHz,
источник CPLL, CPU integer divider=2, root/divider registers одинаковы.
Before/after cache bypass в одном startup тоже ~160 MHz. Прежняя версия
повторно декодировала HOME.JPG за 2.52/2.62 s; после эксперимента возвращён
текущий uncached образ. Частота/PLL не менялись.

IDCT — двухпроходные матричные суммы с Q14, i32 coefficients и i64 accumulators,
1024 MAC terms/full block; DC-only path уже есть. Дизассемблер содержит MUL/MULH,
carry/shift integer operations; f32/f64/software floating point и trig нет.
[Полные данные, ограничения исторического сравнения и логи](media.md).

## Loeffler IDCT — отдельный эксперимент

Матрица сохранена и остаётся default; --jpeg-idct-bench сравнивает L–M–M–L.
На одинаковой сборке/частоте/памяти полный decode 7.355→4.845 s (-34.1%),
IDCT 4.246→1.738 s (2.44×). DC-only 264/14592=1.81%. Все 933888 pre-color
samples Rust Loeffler совпали с pinned C oracle, UBSan clean; 1156 stress blocks
тоже без переполнений. От матрицы максимальное отличие sample=1, RGB565 channel
steps <=1; битового совпадения между разными алгоритмами нет. Оба результата
на плате совпали со своими host hashes. Кеши/частота/периферия не менялись,
LCD active, stack peak 10812/16384 B, guards=true. 37 host tests проходят.
[Реализации, границы арифметики, лицензия, команды](idct.md).

## L1 I-cache без выделенного L2

Подтверждено на rev1.3: runtime core0 I-cache ON, D-cache/L2 bypass, L2 size=0.
Matrix JPEG 7.630 → 3.764 s, Loeffler 4.964 → 2.462 s при 160 MHz, одинаковые
framebuffer hashes и целые guards; 496 KiB арена сохранена. Default boot по-прежнему
all-bypass. Образ 135552 B. [Результаты и hardware counters](icache.md).

## Контроль L2=128 KiB и L1 D-cache

L2=128 KiB не даёт выигрыша для JPEG из SRAM: счётчики L2 нулевые; отдельная
read-only PSRAM probe подтверждает, что L2 работает. L1 I+D при L2=0 дают
Matrix 2.628 s / Loeffler 1.696 s, 496 KiB арена сохраняется. SD DMA cache
maintenance согласован с P4 ROM patch (двойной writeback/invalidate и DONE).
Default образ 137520 B; после reset all-bypass, runtime оставлен I ON / D OFF.
Typed DMA-публикация при D-cache ON пока запрещена. [Исследование](cache-matrix.md).
