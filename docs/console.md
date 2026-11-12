# Консоль v1

Команда — полная ASCII-строка `:имя аргументы` с LF или CRLF. Двоеточие
принимается только в начале строки. Например: `:display status`.
Одиночные буквы, старые сочетания вроде `:image` и текст логов не выполняются.
Повреждённая/слишком длинная/просроченная строка отбрасывается целиком до LF,
после чего можно отправить новую. Максимум 96 байт после `:`, срок сборки 2 с.
Парсер неблокирующий: touch и основной цикл продолжают работать при вводе.

`:help` выводит полный список и завершается `CONSOLE HELP END`.
Неизвестная команда возвращает `COMMAND ERROR`. Диагностика и результаты
операций остаются текстовыми; окончание операции обозначается её собственным
маркером (`RTC DONE`, `MEDIA IMAGE DONE`, `WEB RUN DONE` и т.д.). Это пока
последовательный протокол без request ID; одновременно работает один клиент.

Во время длительных WAV/web/стресс-операций ESC остаётся отдельным сигналом
остановки. В обычном цикле он не запускает действий. Полные новые команды
во время занятой операции не обрабатываются: сначала остановить её ESC.

## Скрипт

```sh
python3 scripts/display-control.py 'display status'
python3 scripts/display-control.py 'display pattern vertical'
python3 scripts/display-control.py --brightness 25
python3 scripts/display-control.py 'display framebuffer start' --image HOME.JPG
python3 scripts/display-control.py --rtc read
python3 scripts/display-control.py --clock --memory
python3 scripts/display-control.py --web-run 300
```

Позиционные аргументы — полные имена из таблицы ниже, аргументы с пробелами
заключать в кавычки. Параметризованные операции доступны существующими
флагами `--image`, `--play`, `--rtc` и т.д.; их публичные команды указаны ниже.
Скрипт устанавливает raw terminal, снимает DTR/RTS, отправляет LF для
синхронизации строки, затем `:help`. При EAGAIN/частичной записи ждёт USB
до 5 с. Устройство по-прежнему не должно ждать подключённый USB-хост при старте.
Общая карта имён: `commands.tsv`, `command-routes.tsv` — используется и Rust,
и Python. Старые команды буквами в исторических логах больше не являются API.

## Действия без аргументов

| Команда после `:` |
|---|
| `help` |
| `display status` |
| `display errors` |
| `display scanline` |
| `display test bist` |
| `display pattern vertical` |
| `display pattern horizontal` |
| `display pattern ber` |
| `display video 650 rgb565` |
| `display video 750 rgb565` |
| `display video 750 rgb888` |
| `display video 550 rgb565` |
| `display invert on` |
| `display invert off` |
| `display mirror x` |
| `display mirror y` |
| `display mirror off` |
| `display order bgr` |
| `display order rgb` |
| `display off` |
| `display on` |
| `display idle on` |
| `display fill black` |
| `display fill white` |
| `display fill off` |
| `display sleep` |
| `display wake` |
| `display te vblank` |
| `display te both` |
| `display te off` |
| `display te line 300` |
| `display idle-color green` |
| `display reset software` |
| `display reset hardware` |
| `display brightness up` |
| `display brightness down` |
| `display test rgb` |
| `display test chart` |
| `display screenshot` |
| `display thumbnail` |
| `display framebuffer start` |
| `display framebuffer status` |
| `display framebuffer stop` |
| `camera controls` |
| `camera gain digital up` |
| `camera gain digital down` |
| `camera gain analogue up` |
| `camera gain analogue down` |
| `camera exposure up` |
| `camera exposure down` |
| `camera start` |
| `camera start-test` |
| `camera stop` |
| `camera info` |
| `camera capture-test` |
| `camera scene` |
| `audio tone` |
| `sd config-create` |
| `sd list` |
| `sd capabilities` |
| `sd probe` |
| `sd benchmark read` |
| `sd test multi-write` |
| `sd test write` |
| `memory psram-test` |
| `display idle off` |

## Параметризованные операции

| Команда после `:` | Аргументы |
|---|---|
| `display brightness` | 0…100 |
| `display clock` | on / off; автозапуск, время MSK из UTC RTC |
| `display image` | PATH, baseline JPEG 1024×600 |
| `audio play` | PATH, PCM16 WAV 16 кГц |
| `audio record` | PATH SECONDS, новый WAV, 1…600 с; STOP/ESC |
| `rtc` | read / init / set / config / alarm / alarm-ack / alarm-off / timer / ram / clear, см. [rtc.md](rtc.md) |
| `system clock` | без аргументов, только измерение |
| `system cache instruction` / `system cache data` / `system cache l2` | on / off; L2 только l2-128 build |
| `system cache l2-probe` | без аргументов |
| `memory status` / `memory benchmark` / `memory test` | без аргументов |
| `net ethernet probe` | без аргументов |
| `net ethernet run` | 1…600 секунд |
| `net web serve` | 1…86400 секунд |
| `sd format` | без аргументов — preview; `FAT32 ERASE` — стереть карту |
| `sd test dma` / `system test mixed` | без аргументов |
| `jpeg benchmark output` / `jpeg benchmark idct` / `jpeg profile` | PATH |
| `audio test play-stall` | PATH, ожидаемая проверка underrun |
| `audio test loopback` / `audio test underrun` / `audio test control` / `audio test repeat` | без аргументов, feature audio-lab |

BIST, тесты записи SD, переключение кешей, тесты памяти и сбросы запускаются
только явными полными командами. Форматирование сохраняет отдельную фразу
`FAT32 ERASE`. Тесты записи SD сохраняют/восстанавливают секторы при штатном
завершении, но при обрыве питания восстановление не гарантировано.
Ограничения режимов (активный framebuffer, камера, DMA, кеши) сохраняются.

[DMA](dma.md), [web](web.md), [Ethernet](ethernet.md), [RTC](rtc.md),
[audio lab](wav-lab.md). На этом этапе системы команд механизм запуска
драйверов сохранён; изменения касаются приёма, имён и транспорта USB.

Запись микрофона: `--record PATH --record-seconds N`, подробнее [recorder.md](recorder.md).

Диагностика арбитража: `:memory arbitration status` и
`:memory arbitration bench PROFILE MODE TARGET ROUNDS`.
Профили `baseline`, `matrix-prio`, `matrix-qos`, `combined`,
`wrong-port-qos` (контроль), `postwrite-off`, `cutthrough-off`;
MODE=`u16|u32|paced`, TARGET=`frame|scratch|linear`, ROUNDS=1..200.
Тест может намеренно вызвать видимые срывы LCD; сохраняет и восстанавливает
пиксели и изменённые настройки. Условия, метрики и ограничения:
[memory-arbitration.md](memory-arbitration.md).

Для короткого исследования H2X доступны также MODE=`probe|busy|fence-busy`:
одно чтение BUSY / ожидание сброса / fence и ожидание, каждые32 пикселя.
Таймаут одного ожидания160000 тактов. Это диагностические режимы, не новая
штатная политика рисования; см. ограничения измерений в memory-arbitration.md.
