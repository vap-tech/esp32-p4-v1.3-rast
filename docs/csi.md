# CSI: первый RAW10-кадр с OV02C10

Этот документ описывает RAW-пробу `A/o`. Новый поток RAW10 → ISP → полный RGB565
framebuffer в PSRAM описан в [framebuffer.md](framebuffer.md).

Собственный Rust `no_std`, без ESP-IDF runtime, RTOS, allocator или новых Cargo
зависимостей. На ESP32-P4 rev 1.3 проверяется захват **64×32 RAW10 = 2560 B**
в SRAM. Это маленькая вырезка исходного изображения, не масштабированный preview.
На LCD изображение камеры пока не выводится; BER и подсветка 5% остаются работать.

## Подключение и опознание

- Общая I²C1 GPIO7/8, 100 кГц, адрес сенсора `0x36`.
- SCCB: 16-битный адрес регистра big-endian, 8-битное значение.
- Полный ID `0x300A/B/C = 56 02 43`, OV02C10; ранее читались только `56 02`.
- Используются две аппаратные data lanes CSI, VC0, RAW10 DT `0x2B`.
- Сенсор работает без генерации XCLK нашим P4 и без управления reset/pwdn GPIO.
  Источник и точная частота его внешнего clock на этом модуле физически не
  измерялись; рабочая таблица рассчитана на 24 MHz.
- Схема I_W из `old/datasheets/vendor-search/cheops/4_CONN.png` показывает
  **15-контактный CSI FPC**, питание 3.3 V, две data lanes, clock lane и SCCB.
  Это уточняет прежнее неподтверждённое упоминание 30 pin из описи. Точную
  ориентацию FPC и соответствие ревизии платы перед переподключением сверять
  физически. Не назначать произвольные GPIO по именам CSI_IO0/CSI_IO1.
- PHY использует уже настроенный дисплеем общий LDO3 2.5 V; отдельного изменения
  напряжения при захвате нет. Без успешного старта Display диагностическая
  команда откажет: независимое владение общим LDO пока не выделено.

## Путь данных и регистры hw_ver1

`OV02C10 → CSI-2 host → ISP input (bypass) → CSI bridge → DW-GDMA → SRAM`.
Полноценная обработка ISP, demosaic, AE/AWB пока выключены.

| Блок | Адрес / настройка |
|---|---|
| CSI host | `0x5009F000` |
| CSI bridge | `0x5009F800` |
| ISP input | `0x500A1000`, clock PLL240 / 3 = 80 MHz |
| DW-GDMA | `0x50081000`, channel 0 (аппаратное имя CH1) |
| CSI FIFO | `0x50104000`, фиксированный адрес источника |
| HP clocks/reset | `0x500E6000` |
| Cache sync | `0x3FF10000 + 0x98…0xA4`, L1 DCache map bit 4 |

CSI PHY config clock PLL_F20M, HSFREQRANGE `0x25` (400…449 Mbps/lane).
Номинальная скорость — из выбранной таблицы около 400 Mbps/lane; это настройка,
не результат измерения сигнала. Для rev 1.x используются исключительно регистры
`hw_ver1`, возможности bridge rev 3.x не переносятся.

Два важных условия, найденные при запуске:

1. Даже без обработки изображения **нужен ISP input**: `MIPI_DATA_EN=1`,
   `ISP_EN=0`. Без этого clock lane активна, но FIFO/DMA не получают пиксели.
   При bypass горизонтальный размер ISP задаётся в **32-битных словах минус 1**:
   `(width * 10 / 32) - 1`, вертикальный — `height - 1`. У bridge отдельные
   поля используют обычные width/height. Ошибка на единицу давала неполный кадр.
2. Наша программа не настраивает кэши при старте, однако **ROM оставляет L1
   DCache для внутренней SRAM активным**. DMA двигал destination и выставлял
   completion, а CPU продолжал читать прежние `A5`. Volatile/fence не заменяют
   cache sync. Перед DMA нужен writeback+invalidate, после — invalidate.
   После синхронизации пришли реальные RAW10-байты. PSRAM/L2 external mapping
   для этого не требуются.

Буфер выровнен на 64 B; размер кратен 128 B, до/после стоят защитные области
по 64 B. DMA использует 64-битные элементы, источник fixed/M0, назначение
increment/M1, burst 16, фиксированную длину под контролем DMA. Даже если сенсор
пришлёт больше данных, лимит записи не увеличится. Значение `transferred=319`
в диагностике — аппаратный счётчик минус один: 320 × 8 = 2560 B.

## Последовательность и владение

`devices::ov02c10` проверяет ID, записывает таблицу сенсора, оставляя stream off,
и задаёт output crop через `0x3808…0x380B`. Базовые HTS/VTS и PLL сохранены;
реальная частота кадров crop-режима отдельно не измерялась.
Тестовый шаблон включается `0x4503=80`, реальная матрица — `00`.

Приложение включает streaming, выдерживает 100 ms без USB logging, затем
сбрасывает bridge/ISP input и очищает накопленные стартовые host flags.
Это отделяет первоначальный непарный frame boundary от проверяемого кадра.
Затем настраивает размеры/FIFO/DMA, запускает захват и опрашивает completion
до 500 ms. Проверяет DMA, host CRC/ECC/PHY/frame errors, bridge и ISP errors.
При успехе и при ошибке останавливает bridge, останавливает только
свой channel 0 DW-GDMA, синхронизирует кэш; приложение выключает сенсор.

Во время владения DMA нет Rust-ссылки на пиксели или записи в USB. После
остановки байты читаются volatile для подсчёта изменений и FNV-1a.
`status()` читает и очищает host errors; это диагностический, не пассивный API.
`arm()` — unsafe, пока не введена типизированная модель DMA-буфера.
Внешний код обязан удерживать память и её cache lines до `finish`/`stop`.
DW-GDMA разделён: CSI channel 0, LCD channel 1. Глобальный reset DMA из CSI
убран; запуск и остановка RAW-пробы не должны останавливать развёртку LCD.

Команда блокирует основной цикл, включая опрос touch, примерно на время
настройки SCCB + warmup + приёма. Это одиночная проба, не потоковый API.

## Команды проверки

```bash
python scripts/display-control.py a       # read-only sensor info
python scripts/display-control.py A A     # два кадра тестового генератора
python scripts/display-control.py o o     # два кадра с матрицы
python scripts/display-control.py a q e   # stream off, дисплей и его RX errors
```

`CAMERA result=Ok(())`, `start=Ok(())`, `stop=Ok(())`, `guards=true`,
нулевые host errors, `bridge=32` (configuration update, не ошибка),
`depth=0`, `dma=10` (completion + source complete).
ISP raw `0x18000000` — header/tail frame events, не ошибка.

В проверке после warmup два тестовых кадра совпали: `hash=daa35fc5`,
все 2560 B равны `FF`. Crop попадает в светлую область шаблона;
это не доказательство приёма всех цветных полос полного 1920-пиксельного кадра.
Режим матрицы дал отличающиеся данные и хэши при целых защитных областях.
Шум матрицы сам по себе не доказывает полноценное изображение сцены.
Логи этапов: `target/csi-{first,isp-bypass,cache,small-fixed,warmup}.log`;
итоговая серия — `target/csi-regression.log` и `target/csi-check.json`.
В ней прошли **20/20 захватов**: 10 тестовых и 10 с матрицы. Все guards
целы, host errors = 0, FIFO пуст, после серии stream off; у LCD RX errors = 0.
`changed` считает байты, отличные от начального A5, а не длину DMA: настоящий
пиксель тоже может равняться A5. Длина подтверждается DMA completion/count.
Сборка release, clippy с `-D warnings`, fmt и 8 host-тестов проходят.

## Текущее состояние после RAW-проб

PSRAM, полный framebuffer и непрерывный ISP RAW10→RGB565→LCD уже подняты:
[framebuffer.md](framebuffer.md). Ручные exposure/gain проверены, но изображение
камеры пока **очень тёмное**, цветопередача не настроена. AE/AWB/gamma/denoise
не доведены; попытка обработки отменена. Полная сводка сделанного и оставшегося:
[status.md](status.md). Камеру пока больше не меняем по решению пользователя.
Захват 64×32 не означает, что проверен полный поток 1080p.

## Источники

- Локальный ESP-IDF v6.1 `fff9895c82d744c7237be8847347bdd1b07c6643`:
  CSI HAL/LL, CSI controller, DW-GDMA HAL/LL, ISP core/LL и cache register map.
- Таблица OV02C10: открытый [PR #46](https://github.com/espressif/esp-video-components/pull/46),
  head `ed9f3ddc8bff764b02a0b7c22dd7179b99bdb6a3`,
  `ov02c10_input_24M_MIPI_2lane_raw10_1920x1080_30fps`.
  Этот драйвер ещё не принят в основную ветку Espressif. Сохраняем его статус
  как экспериментального источника, хотя crop проверен на нашей плате.
- [Схема разъёма I_W](https://github.com/cheops/JC1060P470C_I_W/blob/main/5-Schematic/4_CONN.png).
- Атрибуция и Apache-2.0: [licenses/README.md](../licenses/README.md).
