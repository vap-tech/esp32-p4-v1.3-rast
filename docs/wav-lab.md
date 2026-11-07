# Live WAV + SD JPEG + LCD: ring margin and continuous digital verification

2026-11-06, ESP32-P4 rev1.3, CPU ~160 MHz, L2=0, uncached PSRAM.
Продолжение [DMA validation](dma.md), без RTOS, IRQ, heap или новых dependencies.
Громкость DAC повышена на **3 dB: reg32 0x8F (-24 dB) → 0x95 (-21 dB)**.
PCM не масштабируется: GPIO9 capture проверяет исходные цифровые сэмплы до DAC.

## Один SD-контроллер, два настоящих файла

`SharedVolume` владеет одной FAT32/SD; приватный RefCell выдаёт короткий mutable
borrow только на время операции. `SharedFile` хранит собственные position и
cluster hints. WAV и JPEG читаются по очереди из **одного** контроллера и общего
тома; WAV не загружается заранее в PSRAM. Курсоры не дают заменить том под собой:
внутренний RefCell не экспортируется, consuming into_inner запрещён borrow checker,
пока курсоры ещё используются. Существующий эксклюзивный File API сохранён.

Новые host-тесты проверяют два чередующихся курсора, rewind/seek, границы кластеров,
независимость позиций и completed-chunk progress после ошибки I/O. При playback
не создаются новые allocations. Wave::read_frames_with_scratch использует caller
scratch, чтобы не размещать дополнительные 2 KiB на глубоком стеке JPEG callback.

## Тестовая сборка

```sh
./scripts/image.sh --features audio-lab
cp target/firmware.bin target/wav-lab.bin
espflash write-bin --port /dev/ttyACM0 0x2000 target/wav-lab.bin
python3 scripts/display-control.py --icache on --dcache off --image HOME.JPG --wav-lab
python3 scripts/display-control.py --dcache on --wav-lab
python3 scripts/display-control.py --wav-lab-repeat
python3 scripts/display-control.py --wav-lab-stall
python3 scripts/display-control.py --wav-lab-control
```

`audio-lab` — необязательный feature для continuous I2S RX capture tests.
С 2026-11-08 stage-2 image сохраняет все обычные команды; feature добавляет
continuous RX capture diagnostics без размерных исключений. DMA allocations
берутся из отдельной нижней арены 152 KiB. Камера не запускалась, её драйвер не менялся. Тест требует активный framebuffer,
выключенную камеру и пустую DMA-арену.

SATIE.WAV — PCM16 mono 16 kHz, раздаётся в оба I2S16 slots; HOME.JPG — baseline
1024×600. Тест декодирует JPEG до истечения минуты, заканчивая текущий кадр.
Каждый decoded pixel stream проверяется по hash 534b3012, framebuffer guards целы.
Это hash вычисленных пикселей: полный PSRAM framebuffer CRC во время playback
не считается, чтобы не блокировать audio service длинным последовательным проходом.

## Непрерывный RX

TX: четыре блока по 512 stereo frames / 2048 B, owner check и auto-writeback ON.
RX0: восемь блоков по 512 frames, GPIO9 (реальный DOUT), общие TX BCLK/WS.
RX data/descriptors используют uncached SRAM alias после initial writeback/invalidate;
каждая descriptor line изолирована. CPU проверяет только OWN=CPU и возвращает
OWN=DMA после проверки. Все allocations удерживаются до подтверждённого park.
Если reset RX не подтверждает park, firmware останавливается, память не возвращается
арене. Guards перед/после RX data и после descriptor storage читаются физически.

Reference history: последние 8192 исходных stereo frames из WAV. Сначала ищется
уникальный 64-frame fingerprint в этой истории. Первый неполный RX word и
неоднозначный префикс не считаются проверенными. В обоих финальных проходах
префикс составил **512 frames / 32 ms**. После выравнивания сравнивается **каждый
следующий frame**, без автоматической пересинхронизации. Любое отличие завершает
тест с выключением PA и запросом остановки TX; затем RX/TX park подтверждаются.
Последний неполный RX block (<512 frames) и короткий интервал между остановками
RX/TX вне сравнения. Это не проверка всего 183-секундного WAV до последнего sample.

## Метрики

- `owned_min`: минимум **наблюдавшегося** числа OWN=DMA, включая current/prefetch.
  Snapshot не атомарен; наблюдения есть на MCU boundaries и перед публикацией.
  Это не непрерывный hardware low-watermark и не гарантированный запас звучания.
  Нельзя просто умножить его на 32 ms и объявить минимальный аудиозапас.
- `latency_lower_us`: max(publish_complete − first_observed_free).
- `latency_upper_us`: max(publish_complete − last_observed_owned). Для OWN=DMA
  timestamp берётся перед snapshot, для OWN=CPU — после: оценки консервативны.
  Это границы максимальной задержки до завершения software publication, а не
  точное время release или физического выхода samples из I2S FIFO.
- `fill_us`: только WAV read/expand/cache handoff, отдельно от ожидания обнаружения.
- `gap_us`: максимальный интервал audio service, не аудиозапас.
- `tx_errors`: любой TX DMA error; `underruns` считается отдельно только при
  descriptor error и полностью истощённом ring (OWN=CPU у всех). Остальные
  descriptor errors не выдаются за доказанный underrun. Каждый инцидент считается
  один раз: после fail service больше не повторяет подсчёт sticky IRQ.

## Финальные результаты — один и тот же бинарь

| Метрика | I ON / D OFF | I ON / D ON |
| --- | ---: | ---: |
| Wall time | 60.363684 s | 61.514618 s |
| JPEG frames | 23 | 34 |
| PCM refills | 1890 | 1926 |
| Последовательно проверенные stereo frames | **965120** | **983552** |
| owned_min observed | **3** | **3** |
| Max refill latency, lower…upper | **2.802…4.893 ms** | **2.761…3.649 ms** |
| Max WAV fill | 2.801 ms | 2.760 ms |
| Max service gap | 6.324 ms | 6.183 ms |
| Underruns / TX errors / sample mismatches | **0 / 0 / 0** | **0 / 0 / 0** |

Итого **1948672 stereo frames** в этих двух финальных проходах после alignment.
BCK и DMA counters активны, RX guards и stack guard целы. Peak stack **10812 /
16384 B**, остаток 5572 B включая canary (результат этой нагрузки, не ISR budget).
В test workspace заранее выделено **53952 B** SRAM для scratch/history/RX/descriptors/
guards. После завершения borrow scopes арена сброшена, used=0, capacity=496 KiB.
Дополнительные первые проходы дали те же выводы; финальные логи перечислены ниже.

## Проверка самих детекторов

`--wav-lab-repeat`: после alignment в одном TX block frame **18952** заменён
повтором предыдущего. Reference history не менялась. Получен точный mismatch:
expected=0x00020002, actual=0x00010001, index=18952, **TX errors=0**. Тест принят
только потому, что место обнаружения совпало с местом инъекции. Без пересинхронизации.
Это демонстрирует выявление повтора/потери ожидаемого кадра, который не дал DMA error.

`--wav-lab-stall`: producer задержан на 200 ms. observed owned_min=0,
underruns=1, tx_errors=1, DMA raw=7, park=1, все OWN=CPU. PA выключен, память
не переиспользуется до подтверждения park. Следующий нормальный прогон прошёл.

Обе fault-команды возвращают Ok только при ожидаемом обнаружении; обычный тест
при таком событии возвращает Err. Это не скрытое восстановление потока.

## Стоимость проверки

Контроль без RX выполняет 8 JPEG с тем же реальным WAV и метриками TX:
20.824893 s (D OFF), 14.304539 s (D ON). В пересчёте на JPEG непрерывный capture
и сравнение добавили ориентировочно **0.8% / 1.2%**. Контроль короче minute run,
не выполнялся ABBA, так что это оценка, не точный paired benchmark. Отдельная
стоимость TX-метрик этим сравнением не изолирована.

## Образы и логи

Диагностический image: **107664 B**, SHA256
`47d499283f74f8ad965343c23de345109a4a198bcb1bae9b63d98b338e7efbfa`.
SharedVolume wrapper после последних API-правок дал побитно тот же image, что
проходил аппаратную серию. Логи в ignored target/wav-lab:

- final-{i-only,i-and-d}.log — непрерывные сравнения;
- final-{i-only-control,i-and-d-control}.log — RX disabled;
- final-{repeat,stall}.log — проверка детекторов;
- final-series.log — все стадии успешно завершены за 169.256 s.

Тестовые бинарники/сырые логи остаются локальными; численные результаты сохранены
здесь. После тестов возвращается обычная сборка, I ON / D OFF, L2=0, PA выключен
после завершения воспроизведения. После reset кеши по-прежнему all-bypass.
Универсальная typed DMA политика и CSI/ISP остаются отдельной работой. Из конечного
теста не следует гарантированный минимум свободного аудио для произвольной нагрузки.

Default image after restoring ordinary commands: **141936 B**, SHA256
`6295fde0adfcfd0cd7ce37e0d3e45deb4fa3dd554230527e3e62883936ac265d`.
39 host tests pass (2 external asset diagnostics ignored); clippy with/without
audio-lab, default and l2-128 release builds, fmt and Python syntax checks pass.

После восстановления default build дополнительно проиграны 20 s SATIE.WAV:
623 refills, max_refill=2240 us, DMA raw=3, stop PA=false/park=1. Runtime оставлен
I ON / D OFF / L2=0, peak stack этого обычного playback 10316 B, guard цел.
Логи default-playback-final.log и default-state-final.log; arena used=0.
