# Display: управление и аппаратные проверки

2026-11-05, JC1060P470C / ESP32-P4 rev 1.3. Публичный интерфейс:
[`devices::jd9165::Display`](../src/devices/jd9165.rs).
Объект владеет `Dsi`, `Output<27>` RESET и `Pwm<23>` подсветкой.
Инициализация выполняет аппаратный сброс и прежнюю vendor-таблицу;
на выходе BER / RGB565 / 550 Mbps/lane, 5% PWM 20 кГц. Аллокатор,
SDK/runtime и внешние зависимости не добавлялись.

```rust
use esp32_p4::{devices::jd9165::{Display, ColorOrder, Reset}, hal::dsi::Pattern};
// dsi/reset/backlight уже созданы с эксклюзивным владением периферией.
let mut display = Display::new(dsi, reset, backlight, &mut timer)?;
display.set_pattern(Pattern::Vertical)?;
display.set_brightness(5);
display.set_mirror(true, false, &mut timer)?;
display.set_color_order(ColorOrder::Bgr, &mut timer)?;
let status = display.snapshot(&mut timer)?;
display.sleep(&mut timer)?;
display.wake(&mut timer)?;
display.reset(Reset::Software, &mut timer)?;
```

## Что поддержано и что именно проверено

| API | Аппаратный результат |
|---|---|
| `snapshot()` | ID 04h, статус 09h (4 байта в порядке передачи), power 0Ah, MADCTL 0Bh, image 0Dh, signal 0Eh, self-diagnostic 0Fh читаются |
| `set_pattern(Vertical/Horizontal/Ber)` | Все VPG-режимы переключаются; полосы и BER ранее визуально подтверждены пользователем |
| `set_brightness(0..=100)` / `pwm_duty()` | PWM 20 кГц, 10 бит; ранее измерены GPIO_IN уровни 0/25/50/75/100%, 5% визуально подтверждены; текущий duty=51 |
| `set_inverted(bool)` | image 00→20→00; соответствующий бит меняется и в 09h |
| `set_mirror(x,y)` | MADCTL 0Bh: X=02, Y=01, normal=00; readback 09h также меняется |
| `set_color_order(Rgb/Bgr)` | У BA в байте 0 статуса 09h бит 2 меняется: 80↔84 |
| `set_display_on(bool)` | Power 14→10→14; PWM остаётся 5%, проверяется LCD-команда, не выключение света |
| `set_idle(bool)` | Power 14→54→14; дополнительно статус 09h меняет idle-бит |
| `set_idle_color(r,g,b)` / `idle_color()` | Зелёный записан/прочитан как 02; в idle `first_pixel()` дал `[00,FF,00]` |
| `set_test_fill(Some(Black/White))` | 09h byte 2: normal=04, black=0C, white=14 |
| `set_test_fill(None)` | Возврат к обычному изображению через hardware reset + vendor init с восстановлением настроек |
| `set_tear_effect(Off/Vblank/VblankAndHblank)` | Signal 0Eh: 00/40 (off), 80 (Vblank), C0 (both) |
| `set_tear_scanline(0..600)` | Программируется 44h, включается Vblank TE; line=300 принят, 0Eh=80. Сам порог линии и физический TE не измерены |
| `sleep()` / `wake()` | Сон: power=00, duty=0; пробуждение: power=14, duty=51. Если до сна display был off, после wake остаётся power=10 |
| `reset(Hardware/Software)` | Обе последовательности проходят, настройки восстанавливаются |
| `take_packet_errors()` | Четыре чтения после проверок link modes дали count=0, overflow=false; чтение очищает счётчик по manual, fault injection не выполнялся |
| `first_pixel()` | 06/07/08h читаются; BER дал `[AA,AA,AA]`, idle-green — `[00,FF,00]` |
| `host_status()` | Диагностика P4, включая ошибки, уже прочитанные при проверке ответов |

Обратное чтение подтверждает изменение регистров. Визуальное действие новых
команд (инверсия, зеркала, заливки, RGB/BGR, idle) ещё требует подтверждения
пользователем. TE на плате не подключён к известному GPIO; проверка битов
не является измерением TE-сигнала. Mirror относится только к изображению:
преобразование координат GT911 приложение выбирает отдельно.

`Snapshot` предоставляет `awake()`, `display_on()`, `inverted()`, `idle()`,
`mirror()`, `color_order()`, `tear_effect()`. Декодирование idle и RGB/BGR
основано на фактическом BA readback; raw-поля остаются доступны из-за отличий
от preliminary manual A-A. Значения self-diagnostic наблюдались 00 и C0;
они выдаются без интерпретации как «всё исправно/неисправно».

## Скорости и глубина цвета

`Display::set_video_config(LaneRate, PixelFormat, timer)` управляет текущим
**тестовым видеопотоком VPG**, не framebuffer/DMA. Поддержаны `Rgb565`,
`Rgb888` и `Mbps550`, `Mbps650`, `Mbps750`. Программируются PHY PLL,
escape/timeout dividers, host color coding и горизонтальные времена.
PLL reference 20 МГц, N=4, M=110/130/150, HS range=17/18/19.
Если новая настройка PLL не проходит, выполняется попытка возврата прежней;
неудачный возврат даёт `LinkRecovery` и требует восстановления host.

| Режим в аппаратной пробе | Color coding | HLINE | CLKMGR | Чистых снимков host |
|---|---:|---:|---:|---:|
| RGB565, 650 Mbps | 0 | 2178 | 0805h | 2 |
| RGB565, 750 Mbps | 0 | 2513 | 0905h | 2 |
| RGB888, 750 Mbps | 5 | 2513 | 0905h | 3 |
| Возврат RGB565, 550 Mbps | 0 | 1843 | 0704h | 2 |

На каждый режим приходилось около 12 секунд прогона; PLL lock=1,
errors0/errors1=0 в указанных снимках, приёмный 05h=0 после каждого режима.
Перед первым переключением в логе был сохранён флаг ECC от отдельного
эксперимента 45h; он не относится к этим чистым снимкам. Это короткая
проверка генератора и регистров, без внешнего измерения PHY и без нагрузки
PSRAM/DMA. Не объявляет частоту/качество произвольного renderer проверенными.

## DSI-команды во время видео и восстановление

Включены LP-окна только вне VACT: `DPI_LP_CMD_TIM=0x00100000`, максимум
16 байт пакета в строке vertical blanking. `LP_CMD_EN` без этого регистра
недостаточен. Источник: локальный ESP-IDF v6.1
`mipi_dsi_host_ll_set_lp_largest_cmd_packet_size()` и
[описание того же интерфейса DSI](https://onlinedocs.microchip.com/oxy/GUID-51CE727C-126C-4670-A2BA-189781F25924-en-US-2/GUID-C016FA8A-5CB3-40E6-A5B8-C44732CFED69.html).
Сами команды используют существующий VC0, LP и BTA. Видеопоток при
обычном чтении/записи не останавливается.

`write_dcs` допускает 63 параметра в command mode, **9 в live video**
(байт команды + параметры + long header/CRC должны поместиться в слот).
Больший запрос отклоняется до заполнения FIFO. Vendor init с большими
массивами выполняется при выключенном видео.

После опустошения TX FIFO при runtime-записи выдерживается 1 мс: FIFO empty
ещё не означает завершение физического LP-пакета/ACK. Изначальная попытка
немедленно перезапускать host после FIFO empty давала `Ok` без изменения
регистров; это исправлено и проверено обратным чтением.

`read_dcs` ограничен 1…4 байтами и ожиданиями по 100 мс. Старый RX дренируется
ограниченным циклом, лишние слова вызывают `ReceiveFifo`. Некорректируемая
ECC/CRC/packet-size/EoTp/RX ошибка даёт `ReadResponse {host0,host1}`.
Исправленная single-bit ECC разрешает использовать ответ, но остаётся
видимой в `host_status()`. Накопленные флаги — bitset, не счётчик событий.
После ошибки host сбрасывает command/RX состояние, ждёт 20 мс и убирает
запоздалые RX/ошибки в накопленную диагностику. Следующие нормальные
чтения восстанавливаются; ошибка не маскируется возвратом фиктивного значения.

## Сон, сбросы, границы поддержки

Sleep: подсветка 0 → DISPOFF → 40 мс → SLPIN → ещё 40 мс видеотаймингов
→ остановка video → 80 мс. Wake: video → 20 мс → SLPOUT → 120 мс →
восстановление настроек и света. Reset возвращает панель в awake-состояние;
software reset делает SWRESET+120 мс и vendor init, hardware reset — GPIO27
и тот же init. Сохраняются pattern, формат/скорость host, яркость,
зеркала/цветовой порядок, inversion/idle, display on/off, TE/scanline и idle color.
Тестовая black/white override сбросом снимается.

Изменения изображения во сне возвращают `Error::Asleep`, яркость можно
задать заранее — она применится при wake. Scanline для TE >=600 отклоняется
как `InvalidScanline`. TE перед программированием отключается; после
44h принудительно задаётся запрошенный режим, чтобы не зависеть от
различий A/BA в действии этой команды.

- **45h / `scanline()` остаётся экспериментальной пробой.** В live video
  получена ошибка некорректируемой ECC (INT_ST1 bit3), ранее — таймауты и
  недостоверные значения. Две пробы с последующим `q` проверили восстановление.
  Для VSYNC/синхронизации этот метод использовать нельзя.
- **BIST на нашем экземпляре подтверждён:** пользователь увидел автономную
  смену шахматки, градиентов, цветных заливок и других тестов при остановленном
  video P4. Доступна ограниченная `probe_panel_bist()` / команда `B` с reset
  и восстановлением через 25 секунд. Ручной выбор паттерна и выход без reset
  пока не установлены. [Подробности](display-missing.md).
- RGB666, захват физического TE, GRAM и команды графических примитивов
  не предоставляются. Framebuffer/PSRAM/DMA — отдельный этап.
- Блокирующая USB-консоль может приостановить цикл приложения без читающего
  хоста; установленный VPG и PWM продолжают работать аппаратно.

## Как повторить

Остановить другой монитор перед прошивкой/управлением. Скрипт использует
стандартную библиотеку Python/Linux, освобождает DTR/RTS и ждёт ответ каждой
команды. Таймаут транспорта ограничен; `--hold` задаёт время наблюдения.

```bash
./scripts/flash.sh /dev/ttyACM0
python scripts/display-control.py '?'
python scripts/display-control.py --hold 2 v i I x n y n c C d D j J k f N s w b q e
python scripts/display-control.py --hold 6 6 e 7 e 8 e z e
```

| Команды | Действие |
|---|---|
| `B` | Эксперимент BIST, 25 секунд без video P4, затем reset/восстановление |
| `q`, `e`, `L` | status+pixel+idle color; счётчик ошибок (очищает); эксперимент 45h |
| `v/h/b` | Vertical / Horizontal / BER |
| `i/I`, `x/y/n`, `c/C` | Inversion on/off; mirror X/Y/none; BGR/RGB |
| `d/D`, `j/J` | Display off/on; idle on/off |
| `k/f/N` | Black / white / восстановление нормального изображения |
| `s/w`, `r/R` | Sleep/wake; software/hardware reset |
| `t/u/T`, `l`, `g` | TE Vblank/both/off; TE line 300; idle color green |
| `6/7/8/z` | RGB565@650 / RGB565@750 / RGB888@750 / RGB565@550 |

Логи в `target/`: `display-functions-full.log`, `display-functions-final.log`,
`display-link-modes.log`, `display-functions-recovery.log`;
машинная проверка readback/reset/sleep/link modes — `display-functions-check.json`.
В промежуточном `display-functions-final.log` после 45h ещё был один
запаздывающий ECC-сбой; это исправлено и отдельно проверено в recovery log.
Host-тесты прежних драйверов — 8 passed, fmt/clippy/release проходят.

Дополнительный поиск дал схему близкого варианта I_W и экспериментальную
зацепку для программного BIST: [display-missing.md](display-missing.md).
Проба BIST доступна отдельно; визуальное подтверждение пользователя получено.
