# microSD: native SDMMC slot 0, ESP32-P4 rev1.3

## Поднято на плате

Собственный `hal::sdmmc::Sdmmc`, без SDK runtime, Cargo-зависимостей, SPI или
RTOS. Один источник SDIO PLL200: init **400 kHz**, default **4-bit/25 MHz**,
автоматически согласованный **High Speed 50 MHz**, питание 3.3 V.
История прежних тестов от PLL160 на 20/40 MHz сохранена ниже.
Доступ к логическим секторам 512 B: `read_sector` и `write_sector`.
Запись ждёт READY/TRANSFER через CMD13, а не только окончания передачи по шине.

| Линия | GPIO |
| --- | --- |
| D0 | 39 |
| D1 | 40 |
| D2 | 41 |
| D3 | 42 |
| CLK | 43 |
| CMD | 44 |

Это штатный IOMUX function 0, SDMMC slot 0. CMD/DAT имеют внутренние pull-up,
CLK без pull-up, drive strength 3. Отдельных CD/WP GPIO нет; входы заведены на
константы «карта вставлена / запись разрешена». Hotplug не реализован.

**Питание SD pad domain VDDPST_5 — LDO4 / VO4**, PMU ext_ldo[4], регистр
0x501151D8. Включены software ownership, питание и bypass на входные 3.3 V.
Без этого CMD8 отвечал timeout; включение LDO4 восстановило ответы карты.
LDO2 PSRAM и LDO3 DSI/CSI не меняются, переключения UHS 1.8 V нет.

## Карта в текущем тесте

- Заявленная ёмкость по CSD: **15 646 720 секторов = 8 011 120 640 B**.
  Совпала с размером, показанным USB-кардридером ноутбука. Вся ёмкость не
  проверялась на подделку: ноутбук проверил данные первых 2 GiB тестового файла.
- Native OCR 0xC0FF8000, RCA 22964.
- MBR signature 55 AA, первый раздел с LBA **2048**.
- FAT32 boot sector: label **WEBRADIO**, sector 512 B, cluster 8 sectors / 4 KiB.
- MBR в 1-bit/400 kHz и 4-bit/20 MHz совпал побайтно.
- Два чтения по 1 MiB дали одинаковую FNV-подобную контрольную сумму
  `33fe59c4` (вычисляется по 32-битным словам).
- Запись тестового шаблона в последний сектор LBA **15646719** проверена
  чтением. Исходный сектор сохранён в RAM, восстановлен и прочитан обратно:
  `WRITE_TEST Ok(true)`, `WRITE_RESTORE matches=true`.

Карта не форматировалась. FAT32 только распознана по заголовку — файлового
драйвера, монтирования, поиска каталогов и создания файлов пока **нет**.

## Как работает драйвер

- SDMMC base **0x50083000**. Bus clock HP_SYS_CLKRST SOC_CLK_CTRL1 bit14,
  reset LP_CLKRST HP_SDMMC_EMAC_RST_CTRL bit28.
- Источник SDIO PLL200 / 2 = 100 MHz; делитель карты 125 для 400 kHz,
  2 для 25 MHz, 1 для 50 MHz (card divider N означает деление на 2N).
  Это расчётная частота, осциллографом не измерялась. Общие регистры clock
  изменяются масками, сохраняя поля DSI.
- CMD0 → CMD8(1AA) → CMD55/ACMD41(HCS) → CMD2(CID) → CMD3(RCA) →
  CMD9(CSD) → CMD7(select). Затем CMD55/ACMD6(2) и 4-bit host CTYPE.
- Пока поддерживаются карты SD v2 с block addressing, CSD v2 (SDHC/SDXC).
  SDSC/CSD v1, MMC и SDIO не поддержаны. SCR, SD Status и CMD6 mode 0 читаются. Переключение CMD6 High Speed реализовано через configure_bus (см. ниже).
- CMD17/CMD24 по одному сектору. **Встроенный IDMAC SDMMC**, не DW-GDMA,
  которым пользуются камера и дисплей. Один 64-byte-aligned descriptor и
  512-byte SRAM buffer с выравниванием 64 B на стеке, явные L1 cache writeback/invalidate.
- Перед возвратом из функции IDMAC обязательно остановлен. Если reset DMA
  не завершается, panic предотвращает освобождение памяти, которую DMA ещё
  мог бы использовать. Все обычные ожидания имеют ограничения по времени.
- Установлены маски событий IDMAC, чтобы защёлкивались completion-флаги;
  глобальные прерывания SDMMC и CPU выключены, обработка через polling.
- Первый PIO-вариант дал нули при непустом FIFO; он удалён. Получение DATA_OVER
  само по себе не считается доказательством чтения: проверяются IDMAC completion,
  владение дескриптором, сигнатуры, повторяемость данных.

## Команды

```sh
python scripts/display-control.py Y       # SCR/SD Status/CMD6 CHECK, 1-bit/400 kHz
python scripts/display-control.py M       # read-only init, MBR/FAT header, 1MiB на 25MHz → HS/50MHz → 1MiB
python scripts/display-control.py '@'     # raw write/read/restore последнего сектора
```

`M` заново инициализирует карту; сам HAL не требует init перед каждым сектором.
`@` — диагностическая запись вне файлового API, с восстановлением исходного
сектора после попытки теста. Это не атомарная операция: backup живёт в RAM
только до reset. Автоматически при загрузке ни чтение, ни запись не запускаются.

## Скорость на ноутбуке, без Linux page cache

2026-11-05, USB-кардридер MassStorageClass, USB link 5Gbps,
раздел FAT32 WEBRADIO. Файл 2 GiB, блоки 4 MiB, `O_DIRECT`; запись включает
финальный fsync, чтение сверяет все данные. Случайные тесты по 10 s, queue depth 1,
блок 4 KiB. Контроллер карты может иметь собственный буфер; O_DIRECT обходит
именно файловый кеш ОС. Тестовый файл удалён, каталог/ФС синхронизированы.

| Операция | MiB/s | Дополнительно |
| --- | ---: | --- |
| Последовательная запись | 10.93 | 11.46 MB/s, 187.36 s / 2 GiB |
| Последовательное чтение | 21.26 | 22.30 MB/s, 96.31 s / 2 GiB, данные совпали |
| Случайное чтение 4 KiB | 4.62 | 1184 IOPS, p50 0.81 ms, p99 1.17 ms |
| Случайная запись 4 KiB | 0.73 | 187 IOPS, p50 5.55 ms, p99 12.61 ms |

Логи: `target/sd-benchmark.log`, JSON `target/sd-benchmark.json`. Размер файла
меньше FAT32-предела 4 GiB. Форматировать для теста не потребовалось.

## Скорость на плате и оставшееся

На 4-bit/20 MHz первые два 1MiB-прохода: **1170–1171 KiB/s ≈ 1.14 MiB/s**.
Это текущая реализация CMD17 с настройкой/остановкой DMA, cache sync, копированием
512 B и checksum на каждом секторе; **не предел карты или 4-битной шины**.
Время одного write+read тестового сектора около 70 ms, это не тест устойчивой
скорости записи. Нужны многоблочные CMD18/CMD25 и цепочки дескрипторов прежде,
чем сравнивать максимальную пропускную способность с USB-кардридером.

Осталось: многоблочный I/O, бенчмарк записи на выделенном тестовом файле/области,
файловый слой FAT32, автоматическое восстановление после ошибок/извлечения,
долговременные и совместные нагрузочные проверки, high-speed режимы при
необходимости. Работа над камерой по прежнему отложена.

Проверки: fmt, clippy -D warnings, 9 host-тестов (включая декодирование CSD
по фактическому ответу карты и размеру USB-кардридера). Аппаратные логи:
`target/sdmmc-idmac-events.log`, `target/sdmmc-read-bench.log`,
`target/sdmmc-write-restore.log`. Файлы target локальные, не входят в Git.

Источники: локальный ESP-IDF v6.1, `components/esp_hal_sd/esp32p4/include/hal/sdmmc_ll.h`,
SDMMC hw_ver1 registers, `esp_driver_sdmmc/src/sd_host_sdmmc.c`, LDO LL,
ESP32-P4 rev1.3 TRM chapter 52. Это справочные исходники; SDK не линкуется.
Атрибуция: [licenses/README.md](../licenses/README.md).

Финальный образ **67 328 B** прошит на плату. Повторены @ и F/M/S/q/e:
write/read/restore=true, M=Ok(true), 1MiB checksum совпал в обоих проходах,
при активном LCD scanout guards=true, DSI underrun=0 и packet errors=0.
Лог `target/sdmmc-display-final.log`. На дисплее оставлены CPU-полосы из
framebuffer; камера не включалась и её код/настройки в этой работе не менялись.


## Опрос объявленных возможностей — 2026-11-06

Добавлена отдельная команда Y, без записи секторов и переключения скорости:
после init остаётся **1-bit/400 kHz, 3.3 V**. ACMD51 читает 8-byte SCR,
ACMD13 — 64-byte SD Status, CMD6 **mode=0** / argument 0x00FFFFF1 проверяет
High Speed function group 1. CMD6 mode=1 и CMD11 не отправляются.
Общий IDMAC-путь теперь поддерживает блоки данных 8/64/512 B.

Фактические ответы этой карты:

| Поле | Объявлено |
| --- | --- |
| SCR | `02 35 80 01 00 00 00 00` |
| SD specification | SD_SPEC=2, SD_SPEC3=1, SD_SPEC4=0 → SD 3.0x |
| Ширина | SCR mask=0x5 → 1-bit и 4-bit |
| CMD23 SET_BLOCK_COUNT | Не объявлен |
| CMD20 Speed Class Control | Объявлен |
| SD Status speed class | code=4 → C10 |
| Allocation Unit | code=9 → 4 MiB |
| UHS speed grade | 0 (не указан) |
| CSD TRAN_SPEED | 0x32 → 25 MHz в базовом режиме |
| CSD command classes | 0x5B5, SWITCH class присутствует |
| Permanent / temporary write protect | Оба выключены |
| CMD6 access mode support | 0x8003: Default/SDR12 и High Speed/SDR25; bit15 — no-change |
| CMD6 maximum current | 200 mA (заявлено картой, не измерение) |
| CMD6 status version / check result | 0 / 1: проверка High Speed положительна |

В полученном при 3.3 V ответе CMD6 биты SDR50/SDR104/DDR50 равны нулю.
UHS 1.8 V не запрашивался через ACMD41 S18R и не переключался через CMD11;
из этого опроса не делаем вывод о фактически проверенном UHS-режиме.
C10 — заявленный класс записи, не обещание скорости любой операции.
Наличие CMD20 не требуется для обычного чтения/записи; отсутствие CMD23
не мешает многоблочным CMD18/CMD25 с остановкой CMD12.

Дважды выполнено Y: SCR/SD Status/CMD6 ответы совпали полностью, ошибок нет.
Логи: `target/sdmmc-capabilities.log`, `target/sdmmc-capabilities-repeat.log`.
Этот отдельный опрос не переключает High Speed; последующее переключение
проверено ниже на 40 MHz, а не на 50 MHz.
Финальный размер образа с опросом — **68 304 B**. Проверки fmt, clippy -D warnings
и 9 host-тестов прошли. Камера не запускалась и не изменялась.


## История: первый High Speed от PLL160 — 2026-11-06

`Sdmmc::configure_bus` читает SCR и выбирает 4-bit только при поддержке картой,
иначе остаётся на 1-bit. Базовая рабочая частота 20 MHz. Дальше:

1. Проверяются SCR SD_SPEC и CSD SWITCH command class.
2. CMD6 mode 0 проверяет поддержку access function 1, а не просто любой
   ненулевой бит в маске (bit15 no-change не означает High Speed).
3. Для status version 1 ожидается снятие busy function 1, максимум 500 ms.
4. При поддержке отправляется CMD6 mode 1, argument **0x80FFFFF1**.
   Подтверждение selected function=1 обязательно.
5. Только после подтверждения частота повышается до **40 MHz**, Info.high_speed=true.
6. SCR перечитывается и сверяется побайтно. При ошибке/несовпадении частота
   возвращается на 20 MHz и API возвращает ошибку. Карта при этом может остаться
   в High Speed с медленным clock, что отражено в Info.high_speed/clock_hz.

Если High Speed не объявлен, CMD6 mode 1 не отправляется и остаётся обычная
скорость. Это ветвление проверено host-тестом для SD 1.0, отсутствующего SWITCH
class и маски 0x8001 (Default + no-change). На физической карте без HS пока
не проверялось: имеющаяся карта объявляет 0x8003.

**Почему 40, а не 50 MHz:** текущий источник PLL160/2=80 MHz и целый card divider
дают 80/(2×1)=40 MHz. 50 MHz — предел режима карты, не обязательная частота.
Отдельный SDIO PLL200 не поднят; UHS/1.8V и напряжения не меняются.

M теперь сравнивает один и тот же 1 MiB до и после согласования:

| Режим | Время / 1 MiB | Скорость | Checksum |
| --- | ---: | ---: | --- |
| 4-bit/20 MHz, Default | 873210 us | 1172 KiB/s | 33fe59c4 |
| 4-bit/40 MHz, High Speed | 857466 us | 1194 KiB/s | 33fe59c4 |

Ускорение пока около 2%: передача по-прежнему CMD17 на каждый сектор с
перенастройкой DMA/cache sync, а не многоблочная. Это не потолок карты.
При высокоскоростной записи через @ тестовый сектор совпал, исходный сектор
восстановлен и проверен. Тест повторён при активном LCD framebuffer: guards=true,
DSI underrun=0 и packet errors=0. Камера не включалась.

Логи `target/sdmmc-high-speed.log`, `target/sdmmc-high-speed-final.log`.
После финального M карта оставлена в High Speed 4-bit/40 MHz.
Y сохраняет прежнюю роль: только опрос на 1-bit/400 kHz без переключения.
Образ **69 200 B**, fmt/clippy и 10 host-тестов проходят.


## Текущий источник: SDIO PLL200 с самого init

По решению пользователя SDMMC теперь всё время использует отдельный SDIO PLL,
без зависимости от PLL160. После включения LDO4 и bus clock:

- PMU_RF_PWC 0x5011515C bit25 включает питание SDIO PLL.
- PMU_IMM_HP_CK_POWER 0x501150CC получает write-one команды bits20/26/30:
  global clock gate, PLL I2C power, PLL power.
- LP_CLKRST_HP_CLK_CTRL 0x50111040 bits21…23 включает три выхода PLL.
- После 100 us propagation delay выбирается SDIO source (HP 0x500E6034 bit23),
  host divider=2, phase settings как в SDK. Все изменения общих регистров —
  масками, соседние DSI clock fields сохранены.
- Один источник даёт 200MHz / 2 / (2×N): **400kHz при N=125**, **25MHz N=2**,
  **50MHz N=1**. Частоты расчётные по SDK/делителям, не измерение осциллографом.

Первая попытка ожидать ANA_PLL_CTRL0.SDIO_PLL_CAL_END (bit4) завершалась
таймаутом: на этом rev1.3 чипе флаг остаётся нулём. SDK при включении SDIO PLL
не использует его, а ждёт короткую задержку распространения clock. Поэтому
не используем CAL_END как критерий работоспособности: источник проверяется
реальными командами карты с ограниченными таймаутами. Поле Error.pll_state
сохраняет этот регистр только для диагностики. Не заявляем, что причина
поведения CAL_END выяснена или что флаг вообще неприменим ко всем rev1.x.

Проверено на плате:

- Y: ответы SCR/SD Status/CMD6 CHECK читаются на 1-bit/400kHz.
- M: MBR на 400kHz и 4-bit/25MHz совпадает, CMD6 переключает High Speed,
  SCR после повышения частоты совпадает, MBR не меняется.
- 1MiB на 25MHz: 866317 us, 1182 KiB/s; на 50MHz: 854664 us, 1198 KiB/s.
  Обе checksum=33fe59c4. Драйвер всё ещё односекторный, это не предел шины.
- @ на 50MHz: write/read pattern совпал, исходный сектор восстановлен и
  прочитан обратно; write+read занимал 5457 us (не sustained write benchmark).
- При активном LCD framebuffer: guards=true, DSI underrun=0, packet errors=0.

Логи: `target/sdmmc-sdio-pll-first.log` (ложный критерий CAL_END),
`target/sdmmc-sdio-pll-propagate.log`, `target/sdmmc-sdio-pll-final.log`.
На плате оставлены SDMMC High Speed/4-bit/50MHz и CPU-полосы дисплея.
Камера не включалась, её настройки/код не менялись.

Потребление SDIO PLL **не измерено**, прирост тока не оценён. PLL после
использования SDMMC остаётся включённым, power management пока не реализован.
Для сравнения питания нужен внешний измеритель и одинаковая нагрузка платы
при включённом/выключенном PLL с корректно остановленным SDMMC clock.
Значения PLL160 в предыдущих разделах описывают историю, не текущую схему.
Образ **68 032 B**; fmt/clippy и 11 host-тестов прошли, включая точные делители
400k/25M/50M. Источники power sequence — локальный sdmmc_ll.h v6.1 hw_ver1.
