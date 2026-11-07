# Память: эксперимент без кешей, ESP32-P4 rev 1.3

> Исторические измерения: карта с ареной 496/368 KiB относится к образам до 2026-11-08. Актуальная загрузка, динамическая рабочая арена и отдельный DMA pool — [stage2-boot.md](stage2-boot.md).

Состояние на 2026-11-06: L1 I-cache обоих HP ядер, общий L1 D-cache и L2-cache
**при загрузке переведены в bypass**. L2 cache capacity=0; все 768 KiB L2MEM доступны как SRAM.
Это экспериментальная политика загрузки, а не утверждение о лучшей производительности.
Кеши не используются, но их clocks не выключаются: нужно сохранить доступ к SRAM
и cache-controller registers. Отдельная память L1 не превращается в дополнительную SRAM.
CPU1 остаётся в reset. Code/rodata, данные и стек находятся во внутренней SRAM;
PSRAM используется только через uncached alias. Глобального heap нет.

## Карта размещения

| Диапазон | Размер | Владелец/назначение |
| --- | ---: | --- |
| 30100000…30101FFF | 8 KiB | SPM: entry, trap vector, переключение cache policy |
| 4FF00000…4FF20FFF | 132 KiB | ROM-loaded text/rodata budget |
| 4FF21000…4FF25FFF | 20 KiB | data/BSS и boot audit |
| 4FF26000…4FF27FFF | 8 KiB | boot stack, после старта scratch legacy SD diagnostics |
| 4FF28000…4FF3FFFF | 96 KiB | Сохраняем свободным, включая ROM shared buffers, ROM data/stacks |
| 4FF40000…4FFBBFFF | **496 KiB** | Внутренняя арена, отдельная от data/BSS |
| 4FFBC000…4FFBFFFF | **16 KiB** | Runtime stack, watermark и нижняя контрольная зона 64 B |
| 88000000…883FFFFF | 4 MiB | PSRAM display/screenshot reservation |
| 88400000…89FFFFFF | **28 MiB** | PSRAM arena для больших объектов |

768 KiB физической SRAM не равны свободным 768 KiB: код, данные, стек и сохранённые
области ROM исключены из арены. Download buffers можно исследовать для последующего
освобождения отдельно; сейчас они не перезаписываются. SPM и LP SRAM не добавлены в heap.

PSRAM: AP Memory, 32 MiB, HEX x16 DDR, **80 MHz**. MMU identity-maps все 512 страниц
по 64 KiB. CPU address base=88000000, соответствующий адрес DMA=48000000.
Дисплей по-прежнему использует framebuffer 48000080/88000080 и два guard по 128 B;
screenshot занимает область от PSRAM offset 2 MiB. AHB/I²S PSRAM не предоставляется.

## Порядок запуска

1. `_start` в SPM запрещает interrupts, включает clock SRAM и устанавливает
   boot stack через uncached SRAM alias, не затрагивая ещё не освобождённый верхний банк.
2. `rust_cache_boot` в SPM выключает autoload, сохраняет dirty ROM cache data:
   L1 D-cache writeback, затем L2 writeback, каждый дважды с ожиданием SYNC_DONE.
   Это register sequence из P4 ROM patch ESP-IDF v6.1. Прямой вызов ROM
   Cache_WriteBack_All в первой пробе привёл к зависанию/HP watchdog reset;
   готовая ROM-функция больше не вызывается.
3. Устанавливаются L1 bypass=13h, L2 bypass=20h, L2 capacity=0.
   Выполняются аппаратные fences, boot SP возвращается в обычный SRAM alias.
4. Обнуляется только внутренняя BSS. PSRAM здесь не читается и не обнуляется.
5. BSS loop сохраняет отдельную 16-byte .boot_audit с частотой до/после bypass.
   `init_runtime_memory` проверяет cache layout и соответствие linker symbols
   границам арены/стека в HAL. Затем заполняется только watermark выделенного
   16-KiB runtime stack, SP переносится на 4FFC0000 и вызывается rust_main.
   Полный destructive SRAM test из обязательной загрузки **удалён 2026-11-08**:
   две адресные последовательности уже были проверены при bring-up, теперь
   arena/image data не перезаписываются ради повторения этого эксперимента.
   Ошибки конфигурации/границ по-прежнему останавливают загрузку с MEMORY BOOT FAILED.
6. PSRAM включается и отображается только по явной команде. Дисплей и музыка
   автоматически из файлов не загружаются.

Фактический readback: L1 bypass=13h, L2 bypass=20h, L2 size=0;
L1 size/line registers сохраняют значения 64 KiB/64 B, но кеш обходится.
Текущий ROM config до переключения: L2=256 KiB, L1 D=64 KiB, line=64 B.

### Проверка загрузки верхней SRAM (2026-11-08)

После удаления полного boot-test проверены временные ELF/image segments с
32-byte relocatable assembly function и контрольными словами. Ожидаемые bytes
и выполнение подтвердились по `4ff40000` и `4ff60000`. По `4ff80000` и
`4ffb8000` после обычного старта ожидаемых bytes нет. Это ограничение проверенного
пути прямой ROM image load + cache/SRAM transition, а не доказанная неисправность
верхней SRAM. Точный этап потери содержимого пока не локализован; отдельный
power-cycle test не выполнялся. Ранние пустые USB logs не означают, что ROM
не передавал управление: поздний probe показал работающий rust_main и mismatch.

Затем **после** установки L2=0 вызвана ROM `esp_rom_spiflash_read` (`4fc00158`,
сигнатура из локального ESP-IDF ROM header/ld). Source — 32-byte payload в том же
прошитом image, Flash offset `00024edc`; offset проверен двумя последовательными
линковками. ROM return=0, контрольные слова и выполнение прошли на обоих адресах
`4ff80000` и `4ffb8000`, функция вернула `619db357` для input `12345678`.
Перед копированием I-cache выключен/инвалидирован; исполнение проверялось с
I-cache bypass. Это доказательство механизма Flash -> upper SRAM -> execute,
не полноценный loader произвольных Rust modules и не проверка XIP/PSRAM execution.

Вывод: расширять resident code можно вторым этапом загрузки верхних сегментов
после настройки SRAM/L2. Прямую ROM загрузку во все 512 KiB верхних банков
рабочей возможностью пока не считаем. Временный probe code, native objects,
добавочные linker fragments и static Flash-offset marker **не входят** в обычный
образ/исходники; все артефакты только в ignored `target/memory-layout/`.
Обычный образ возвращён на плату: boot после flash и отдельного hard reset,
cache/linker checks и stack guard проходят, arena 496 KiB свободна. SD Z читает
CONFIG.TXT после remount. 51 host-test passed (2 ignored), clippy без warnings,
default и l2-128 образы собираются. Деструктивные пробы не повторяются при старте.

## Владение и выделение

`src/memory.rs` предоставляет отдельные `Arena<Internal>` и `Arena<External>`:
нулевая инициализация, Layout/alignment, границы, used/high_water, reset всей арены.
Это bump arenas, не универсальные heap: Drop буфера не освобождает место;
reset требует эксклюзивного &mut Arena и невозможен при живых borrowed allocations.
Ни malloc, ни Box, ни внешних зависимостей нет. Арены не Send/Sync.

`Psram::split` единожды разделяет владение: `DisplayMemory` для framebuffer и
`Arena<External>` для хвоста 28 MiB. После split нет владельца с доступным
деструктивным full-memory test или переинициализацией MMU. Нельзя одновременно
использовать обычные Rust references на разные aliases одних физических байтов.

`Buffer<Internal>` можно преобразовать в `AhbDmaBuffer<CpuOwned>` при выполнении
условий адреса и изоляции cache lines. publish передаёт тип DmaOwned без CPU slice,
unsafe reclaim требует фактического освобождения DMA. Это token памяти, а не
владелец канала: HAL, запускающий DMA, обязан удерживать его живым до stop.
В этом эксперименте cache handoff сводится к аппаратному fence. I²S пока использует
свои прежние статические буферы; новые арены и типы в его незавершённое кольцо
не внедрялись. Слышимый WAV не проверялся и не объявляется исправленным.

## Команды

```sh
python3 scripts/display-control.py --memory
python3 scripts/display-control.py --memory-test  # destructive: только до framebuffer
python3 scripts/display-control.py --memory-bench
python3 scripts/display-control.py --image HOME.JPG
python3 scripts/display-control.py --memory-bench # тот же тест при LCD scanout
```

USB: `:memory`, `:memory-test`, `:memory-bench` с newline. Скрипт ожидает MEMORY DONE.
Полный тест PSRAM пишет два различных адресных шаблона по всему объёму, затем
сверяет каждое u32. Шаблон включает старшие биты offset, проверяется и наложение
разных страниц. При существующем framebuffer тест отказывает, чтобы сохранить
изображение и screenshot. Он не форматирует и не пишет SD.

Benchmark выделяет временные 128 KiB внутренней SRAM и 1 MiB из хвоста PSRAM;
после возврата освобождает обе арены целиком. Существующий framebuffer сохраняется,
до/после считается полный checksum и проверяются guards. На старте без framebuffer
команда создаёт его reservation, но не включает scanout. Нельзя использовать этот
diagnostic reset арен при стороннем unsafe-владельце DMA.

## Измерения

Последовательные volatile u32 циклы CPU, release build; числа включают fetch
инструкций, цикл, адресную арифметику. **Это не максимальная пропускная способность
шины и не latency отдельной аппаратной транзакции.** Для dependent test следующее
смещение зависит от прочитанного значения; LCG-перестановка, 65536 зависимых loads.
Большие объёмы/word accesses могут работать эффективнее этих scalar циклов.

Предварительный прогон без framebuffer scanout:

| Scratch / working set | Чтение MiB/s | Запись MiB/s | Dependent ns/load |
| --- | ---: | ---: | ---: |
| SRAM, 2 KiB | 50.66 | 29.01 | 103 |
| SRAM, 128 KiB | 50.86 | 29.06 | 103 |
| PSRAM, 2 KiB | 8.47 | 18.68 | 462 |
| PSRAM, 1 MiB | 8.48 | 18.68 | 462 |

Показатели почти не зависят от размера working set: cache-hit/miss переходов нет.
При этом арбитраж памяти, refresh и конкуренция с DMA остаются: выключенные кеши
не делают PSRAM жёстко real-time. Все 32 MiB PSRAM после отключения кешей прошли
два полных шаблона за **12.97 s** в финальном образе. До отключения, через тот же uncached alias — 12.24 s.
JPEG HOME.JPG после отключения кешей: правильные sample pixels, active=true,
guards=true, около **7.43 s** против прежних 2.72 s (вместе с подготовкой SD).

При активном LCD scanout 1024×600, 48 MHz DPI:

| Scratch / working set | Чтение MiB/s | Запись MiB/s | Dependent ns/load |
| --- | ---: | ---: | ---: |
| SRAM, 128 KiB | 50.86 | 29.06 | 103 |
| PSRAM, 2 KiB | 5.73 | 11.54 | 683 |
| PSRAM, 1 MiB | 5.72 | 11.54 | 675 |

Конкуренция с LCD заметно снижает доступную CPU скорость PSRAM; SRAM в этом
прогоне практически не изменилась. Полный framebuffer checksum до/после
**e5cb9e80**, guards=true, scanout active=true, camera=false. После JPEG и
бенчмарка watermark стека **9392 / 16384 B**, нижняя контрольная зона цела.
Арены вернулись в used=0; peaks SRAM=128 KiB, PSRAM=1 MiB.

FAT32 read-only regression: первый 1 MiB SATIE48.WAV дважды прочитан порциями
8192 и 777 B; hash **e2615809** совпал, CMD Z Ok(true). Файлы SD не менялись.
Образ проверки памяти **120640 B**, прошит в плату.
Позднее добавлена JPEG A/B диагностика; текущий размер — в [status.md](status.md). 33 host-теста + fmt/clippy прошли;
отдельный host diagnostic JPEG требует файла и остаётся ignored по умолчанию.

Логи в ignored target/: memory-before.log, memory-upper-sram.log,
memory-psram32.log, memory-no-cache-boot2.log, memory-no-cache-full-test.log,
memory-no-cache-full-test-final.log, memory-no-cache-idle-final.log,
memory-no-cache-lcd-final.log, memory-no-cache-jpeg-final.log,
memory-no-cache-regression.log.
Предшествующий cached PSRAM benchmark в memory-bench-idle.log остановился на
несовпадении после writeback: чистился только L2, хотя SDK обслуживает L1 и L2.
Cached режим не доводился: по решению пользователя выбран uncached эксперимент.
Возвращать кеши в runtime нельзя, пока reclaimed SRAM принадлежит арене/стеку;
это требует другой linker/startup policy и перезапуска.

## Основания

- [TRM, System and Memory §9.3: 768 KiB SRAM / 0 KiB cache configuration](https://www.espressif.com/sites/default/files/documentation/esp32-p4_technical_reference_manual_en.pdf).
- Локальный ~/esp-idf v6.1, hw_ver1 cache_reg.h, SOC_NON_CACHEABLE_OFFSET_SRAM/PSRAM,
  bootloader.memory.ld.in и esp32p4/memory.ld.in для rev <3.
- esp_rom/patches/esp_rom_cache_writeback_esp32p4_esp32s31.c: double writeback,
  SYNC_DONE; esp_rom/esp32p4 ROM ABI. SDK/C runtime не подключён.

Оставлено на будущее: произвольное освобождение отдельных объектов, фиксированные
пулы по размерам, интеграция DMA-token в периферию, оптимизация copy/burst,
исследование LP SRAM/SPM, возврат кеширования по отдельному запросу.

## Отдельный эксперимент L1 I-cache

Runtime `--icache on` проверен при L2 size=0: примерно ×2 на полном JPEG,
без сокращения арены. D-cache/PSRAM остаются некешируемыми. После reset снова
all-bypass. [Документация эксперимента](icache.md).

L1 I+D при L2=0 также проверены: Matrix 2.628 s, Loeffler 1.696 s.
Для 128 KiB L2 добавлена отдельная feature-раскладка (арена 368 KiB); выигрыш
в SRAM JPEG отсутствует. [Карта памяти, DMA maintenance, результаты](cache-matrix.md).

SD/I2S handoffs и смешанная нагрузка с I+D проверены отдельно: [DMA](dma.md).
Текущая нижняя карта расширяет CODE budget до 132 KiB за счёт переиспользования
загрузочного стека для временного SD scratch; арена 496 KiB сохранена.

The live WAV diagnostic preallocates **53952 B** in the internal arena for PCM
scratch, reference history and RX DMA. This replaces deeply nested scratch on the
call stack: measured peak is **10812/16384 B**. Arena used returns to zero after
confirmed RX/TX park. [Validation and limits](wav-lab.md).
