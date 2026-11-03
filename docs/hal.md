# Минимальный HAL: время, GPIO, USB и watchdog

Реализация в `src/hal/`, доступна через `esp32_p4::hal`, без Cargo-зависимостей, ESP-IDF, аллокатора и RTOS. Проверена на ESP32-P4 rev 1.3, кварц 40 МГц. Точка входа по-прежнему в HP TCM после загрузки ROM с flash `0x2000`; основной код теперь в SRAM, актуальная карта памяти — в [gt911.md](gt911.md); сбросы описаны в [boot.md](boot.md).

I²C-драйвер описан в [i2c.md](i2c.md); актуальная карта памяти после добавления GT911 — в [gt911.md](gt911.md). Ниже сохранены результаты предыдущего этапа проверки таймера/GPIO.

## Модули и владение периферией

| Модуль | API | Область ответственности |
|---|---|---|
| `watchdog` | `unsafe disable()` | RTC WDT, MWDT0/1, flashboot protection и super-watchdog; последовательность перенесена из проверенного runtime. |
| `usb_serial` | `unsafe Console::new()`, `core::fmt::Write` | Прямой USB Serial/JTAG FIFO, перевод LF в CRLF, отправка коротких пакетов через WR_DONE. |
| `time` | `unsafe Timer::new()`, `now_us()`, `delay_us(u32)` | SYSTIMER unit 0, собственная эпоха от инициализации, монотонное время и активное ожидание. |
| `gpio` | `unsafe Output::<PIN>::new_low()`, `set_high()`, `set_low()`, `level()` | Push-pull output GPIO0..31 через IO_MUX/GPIO matrix, чтение физического входа GPIO_IN. |

Конструкторы `unsafe`: вызывающий обязан обеспечить единственное владение периферией и её регистрами clock/reset. Драйверы не `Send`/`Sync` и не клонируются. Методы снимка таймера и записи в консоль требуют `&mut self`, чтобы безопасный код не выполнял две такие операции одновременно. Текущий runtime использует одно ядро с выключенными прерываниями; поддержки конкурентных ISR/драйверов и общего clock manager пока нет.

## SYSTIMER

На этой плате источник — XTAL 40 МГц, фиксированный делитель 2,5: **16 МГц, 16 тиков/мкс**. Это аппаратный источник времени, независимый от количества инструкций в цикле задержки и частоты CPU.

Инициализация явно включает APB и функциональный clock SYSTIMER, выбирает XTAL, сбрасывает модуль, загружает ноль в unit 0 и включает счётчик. Оба CPU stall flags сняты: счётчик не останавливается из-за остановки CPU. Unit 1 и alarms в этом драйвере не используются; модуль целиком зарезервирован за ним.

| Регистр | Адрес / биты |
|---|---|
| HP_SYS_CLKRST SOC_CLK_CTRL2 | `0x500E601C`, SYSTIMER APB enable bit 23 |
| HP_SYS_CLKRST PERI_CLK_CTRL21 | `0x500E6098`, source bit 29 = 0 (XTAL), enable bit 30 |
| HP_SYS_CLKRST HP_RST_EN1 | `0x500E60C4`, reset bit 5 |
| SYSTIMER CONF | `0x500E2000`, register clock bit 31, unit 0 enable bit 30 |
| UNIT0_OP | `0x500E2004`, UPDATE bit 30, VALUE_VALID bit 29 (W1C) |
| UNIT0_LOAD_HI / LO | `0x500E200C` / `0x500E2010` |
| UNIT0_VALUE_HI / LO | `0x500E2040` / `0x500E2044`; HI — младшие 20 бит |
| UNIT0_LOAD | `0x500E205C`, apply bit 0 |

`now_us()` запрашивает аппаратный snapshot, очищая старый VALUE_VALID, ждёт готовности и собирает 52-битное значение из HI/LO. Поэтому перенос между младшей и старшей частью не разрывает чтение. Результат — целые микросекунды; переполнение через `2^48` мкс, примерно 8,9 года. `delay_us()` сравнивает разность по модулю `2^48`, поэтому учитывает это переполнение.

`delay_us()` выполняет busy wait, а не сон. При активном счётчике задержка не меньше запроса, но включает накладные расходы чтения регистров и может увеличиться при будущих прерываниях. Чтение snapshot пока блокирующее; внешнее отключение clock/перенастройка периферии нарушает условия конструктора.

## GPIO и проверка GPIO23

Первый драйвер охватывает **нижний банк GPIO0..31**. GPIO24..27 исключены из конструктора: они связаны с внутренними USB PHY. Остальные номера не означают свободные контакты платы: доступность и конфликты нужно сверять с [board.md](../board.md).

Выход сначала отключается, latch предзагружается нулём, затем выбираются push-pull, IO_MUX GPIO function 1 и matrix signal 256 (`GPIO_OUT`). Output enable берётся из GPIO_ENABLE. Вход включён для readback, подтяжки и sleep selection выключены. Исходная drive strength сохраняется. Состояние pad hold и питание доменов этот драйвер не меняет.

| Назначение | Регистры для GPIO23 |
|---|---|
| Выставить / снять уровень | W1TS `0x500E0008`, W1TC `0x500E000C`, bit 23 |
| Включить / отключить output | W1TS `0x500E0024`, W1TC `0x500E0028`, bit 23 |
| Прочитать уровень pad | GPIO_IN `0x500E003C`, bit 23 |
| Push-pull | PIN23 `0x500E00D0`, PAD_DRIVER bit 2 = 0 |
| Matrix routing | FUNC23_OUT_SEL `0x500E05B4`, signal 256, OEN_SEL bit 10 |
| IO_MUX | GPIO23 `0x500E1060`, MCU_SEL bits 12..14 = 1, input enable bit 9 |

W1TS/W1TC меняют только нужный бит и не перезаписывают выходы остальных пинов. `level()` читает **GPIO_IN**, а не выходной latch.

Приложение этапа проверки GPIO трижды включало/выключало GPIO23 с секундными интервалами. После шестого тика оставляет его low (подсветка выключена). Экран/DSI и PWM пока не инициализированы. Подтверждено чтение уровня GPIO, а не яркость/изображение на дисплее.

## Проверка на плате 2026-11-04

Записан образ **3504 байта** штатным `scripts/flash.sh`; проверка flash успешна. Приложение запущено напрямую через ROM. Собраны 12 последовательных строк:

```text
tick=1 us=1000159 dt_us=1000002 gpio23=1 expected=1
tick=2 us=2000368 dt_us=1000002 gpio23=0 expected=0
...
tick=6 us=6001640 dt_us=1000002 gpio23=0 expected=0
...
tick=12 us=12003382 dt_us=1000001 gpio23=0 expected=0
```

- Все номера tick последовательны, время строго растёт, сбросов между отсчётами нет.
- Каждый запрос `delay_us(1_000_000)` дал измерение `1_000_001…1_000_002` мкс.
- Во всех 12 строках GPIO_IN совпал с ожидаемым значением; проверены оба уровня и состояние low после теста.
- Между первым и последним отсчётами: компьютер `time.monotonic()` — **11,002055 с**, SYSTIMER — **11,003223 с**. Проверка масштаба прошла; доставка USB и планирование ОС не позволяют считать эту разницу калибровкой кварца.
- `cargo fmt --check` и `cargo clippy --release -- -D warnings` проходят.

Сырые результаты: `target/hal-check.log`, `target/hal-check.json`; предыдущая проверка только таймера — `target/hal-timer.log`. Логи не входят в Git. Отдельные испытания многолетнего переполнения, длительного прогона, других GPIO, sleep и ISR не выполнялись.

## Первичные источники

Сверялись определения **hw_ver1** и реализация низкоуровневых операций. Эти файлы используются как документация; код ESP-IDF не линкуется и не устанавливается в проект.

- [Частота SYSTIMER и перевод ticks/us](https://github.com/espressif/esp-idf/blob/v6.1/components/esp_hw_support/port/esp32p4/systimer.c).
- [Операции SYSTIMER](https://github.com/espressif/esp-idf/blob/v6.1/components/esp_hal_systimer/esp32p4/include/hal/systimer_ll.h), [регистры hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/systimer_reg.h).
- [Clock/reset hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/hp_sys_clkrst_reg.h).
- [GPIO hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/gpio_reg.h), [IO_MUX hw_ver1](https://github.com/espressif/esp-idf/blob/v6.1/components/soc/esp32p4/register/hw_ver1/soc/io_mux_reg.h), [GPIO operations](https://github.com/espressif/esp-idf/blob/v6.1/components/esp_hal_gpio/esp32p4/include/hal/gpio_ll.h).
