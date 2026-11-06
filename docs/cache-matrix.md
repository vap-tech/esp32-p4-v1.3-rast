# Контроль L2=128 KiB и L1 D-cache при L2=0

2026-11-06, физическая плата ESP32-P4 **rev 1.3**, CPU около 160 MHz.
Продолжение [I-cache эксперимента](icache.md). Без новых Cargo-зависимостей,
RTOS или heap. Камера/аудио не запускались, SD использовалась только для чтения.
PSRAM/framebuffer постоянно через uncached alias 0x88000000, LCD scanout активен.

## Главная архитектурная поправка

Нельзя считать L2 обязательным промежуточным уровнем при доступе L1 к **SRAM**.
В локальном ESP-IDF v6.1:

- `components/soc/esp32p4/include/soc/soc_caps.h`:
  `SOC_CACHE_INTERNAL_MEM_VIA_L1CACHE = 1`;
- `components/hal/esp32p4/include/hal/cache_ll.h`:
  `CACHE_LL_LEVEL_INT_MEM = 1`, `CACHE_LL_LEVEL_EXT_MEM = 2`.

[Первичный исходник Espressif](https://github.com/espressif/esp-idf/blob/fff9895c82d744c7237be8847347bdd1b07c6643/components/hal/esp32p4/include/hal/cache_ll.h).
Инструкции/данные из L2MEM обслуживаются через L1; промах к SRAM не означает
обязательного обращения к L2-cache. Эксперимент со счётчиками ниже это подтверждает.
Это объясняет, почему можно сохранить все 768 KiB L2MEM как SRAM и использовать L1.

## Безопасная отдельная раскладка для 128 KiB L2

Cargo feature `l2-128` задаёт отдельную карту линкера через build.rs:

| Назначение | L2=0, default build | l2-128 build |
| --- | --- | --- |
| CODE/rodata | 4FF00000…4FF1FFFF | тот же диапазон |
| DATA/BSS, boot stack | 4FF20000…4FF27FFF | тот же диапазон |
| Сохранённый ROM region | 4FF28000…4FF3FFFF | тот же диапазон |
| Арена SRAM | 4FF40000…4FFBBFFF, **496 KiB** | 4FF40000…4FF9BFFF, **368 KiB** |
| Stack | 4FFBC000…4FFBFFFF, 16 KiB | 4FF9C000…4FF9FFFF, 16 KiB |
| L2-cache banks | отсутствуют | 4FFA0000…4FFBFFFF, **128 KiB** |

Размещение соответствует rev<3 ветке Espressif memory.ld.in. На rev>=3 карту
не переносить. Размер L2 меняется **при загрузке из SPM на ROM-safe stack**,
до использования верхних банков. Перед переходом ROM dirty data сохраняются.
Доступная верхняя SRAM проверяется двумя физическими шаблонами через 0x8FF…,
затем заполняется watermark нового стека. Размеры проверены по символам ELF.
Default link.x сохраняет исходную карту. build.rs проверяет маркеры шаблона,
чтобы изменение layout не могло молча перестать применяться.

Кодирование CACHE+0x278: `0x200` означает **128 KiB**, не 512 KiB/bytes.
Обе сборки стартуют с bypass L1/L2; l2-128 сразу резервирует банки кеша,
но включение L2 делается отдельно. Runtime `--l2` не меняет размер/карту памяти.

```sh
# Отдельный образ для L2=128KiB; image.sh теперь принимает cargo build options:
./scripts/image.sh --features l2-128
# До последующих сборок сохранить target/firmware.bin под отдельным именем.
# scripts/flash.sh всегда пересобирает default; экспериментальный образ шьётся явно:
espflash write-bin --port /dev/ttyACM0 0x2000 target/firmware.bin
python3 scripts/display-control.py --icache on --image HOME.JPG --jpeg-idct-bench HOME.JPG
python3 scripts/display-control.py --l2 on --l2-probe --clock --jpeg-idct-bench HOME.JPG
```

## Результат L2: обращений JPEG к нему нет

Один образ 137648 B, SHA256:
`163330589a0d218edb41ef23e60f4e3f18dc41c26089d092b29876ddc4245f98`.
I-cache ON, D-cache OFF, PSRAM uncached; одинаковый стек/код/данные в обоих режимах.
Внутри каждой серии LMML: Loeffler → Matrix → Matrix → Loeffler.

| Метрика | L2 reserved 128 KiB, bypass | L2 enabled 128 KiB |
| --- | ---: | ---: |
| Matrix decode | 3.764560 s | 3.764541 s |
| Loeffler decode | 2.461903 s | 2.462055 s |
| Matrix IDCT | 2.025981 s | 2.026061 s |
| Loeffler IDCT | 0.725875 s | 0.725789 s |

Разница неотличима от разброса измерения. **L2 I/D counters = 0 на всех проходах**.
Увеличивать L2 до 256/512 KiB для этой нагрузки оснований нет; эти размеры здесь
не тестировались. Это не вывод о пользе L2 для кешируемых PSRAM/flash нагрузок.

Положительный контроль `:l2-probe`: при остановленной камере прочитаны 1024 B
стабильного framebuffer через uncached alias, затем четыре раза через 0x48000080.
Кешируемые записи не выполнялись; L2 инвалидируется до/после пробы, чтобы не
оставить второй alias для последующих записей framebuffer.

L2 DBUS0 counters `[hits, misses, conflicts, next-level reads]`:
`[1024, 2366, 0, 16]`, checksum **1057571148 == 1057571148**.
16 чтений следующего уровня соответствуют 16 линиям по 64 B. Это подтверждает,
что L2 реально обслуживает внешнюю память, а не только выглядит включённым по битам.
Сырые hits/misses не трактуем как число уникальных строк или инструкции CPU.

Логи: target/cache-matrix/l2-128-{control,positive}-final.log.

## L1 D-cache при L2=0

Возвращён default layout с полной ареной. Один образ **137520 B**, SHA256:
`ea85c71a93d76bbb5276f86cd88e66858235eae333d82aa8fa520e229470ab41`.
Конфигурация PMA не менялась: используется кешируемый SRAM alias 0x4FF…,
а PSRAM остаётся 0x880… . D-cache включается глобально для допустимых PMA регионов;
это не отдельная настройка только для массивов JPEG.

Все четыре комбинации проверены на одной сборке, полный decode_us:

| I-cache | D-cache | Matrix | Loeffler |
| --- | --- | ---: | ---: |
| OFF | OFF | 7.369745 s | 4.860304 s |
| OFF | ON | 7.294342 s | 4.765387 s |
| ON | OFF | 3.761664 s | 2.456933 s |
| ON | ON | **2.627972 s** | **1.695940 s** |

Повторный I+D после D-only/all-bypass: Matrix **2.627858 s**, Loeffler
**1.695686 s**, hashes/guards снова совпали.

I-cache даёт основной выигрыш в этой нагрузке. Один D-cache при выключенном
I-cache даёт около 1–2%, а при включённом I-cache — ещё около **30%** сокращения
полного времени. Это демонстрирует взаимодействие bottlenecks, а не независимые
складываемые проценты. I+D против all-bypass: **2.80× / 2.87×**.

Пример аппаратных L1 D counters при I+D, Matrix:
`[74523240, 7848, 0, 1962]`; I counters
`[311870067, 4098, 0, 537]`. L2 counters по-прежнему нулевые.

Средний профиль, I-only → I+D, секунды:

| Стадия | Matrix I-only | Matrix I+D |
| --- | ---: | ---: |
| Entropy + dequant | ~0.959 | ~0.509 |
| IDCT | 2.025 | 1.463 |
| Color conversion | ~0.376 | ~0.333 |
| Checksum + output | ~0.237 | ~0.218 |

Loeffler IDCT: **0.723 → 0.531 s**. Это согласуется с ускорением доступа к таблицам,
коэффициентам и рабочим массивам, но не является измерением их отдельных задержек.
Время чёрного ящика JPEG не позволяет вычислить точную latency load/store.

## D-cache, DMA и переключение

- `set_data_cache` исполняется из SPM, **без использования стека** в release
  дизассемблере. Перед выключением делает full writeback дважды, затем bypass и
  invalidate. Перед включением инвалидирует старые линии. Interrupts выключены,
  core1 не запущен, переход выполняется между командами при остановленном DMA.
- SDMMC использует отдельный bounce-buffer 8192 B и дескрипторы, выровненные и
  изолированные по 64 B. До передачи — writeback+invalidate, после остановки DMA —
  invalidate. До копирования данных CPU проверяет возвращённые дескрипторы.
- Исправлена операция maintenance SD: ожидание DONE, writeback/invalidate дважды,
  как в ESP-IDF v6.1 `esp_rom_cache_writeback_esp32p4_esp32s31.c`. Сравниваемые
  режимы используют одно и то же исправление. Чтение свежих данных проверено
  полным JPEG и совпадением hashes во всех проходах, включая повторные открытия SD.
- Typed `AhbDmaBuffer` пока **отказывает** при включённом D-cache: его общий
  протокол cache maintenance не расширяли. Проверка относится к JPEG/SD/LCD,
  не ко всем будущим DMA-клиентам. Аудио и камера остаются вне эксперимента.
- Обычная загрузка сохраняет all-bypass. Runtime-команды экспериментальные,
  новый default startup policy не выбран.

```sh
# Default build, L2=0; после --image framebuffer уже инициализирован:
./scripts/flash.sh
python3 scripts/display-control.py --icache on --image HOME.JPG
python3 scripts/display-control.py --dcache on --clock --memory --jpeg-idct-bench HOME.JPG
python3 scripts/display-control.py --dcache off
```

Консоль: `:dcache on/off`, `:l2 on/off`, `:l2-probe`; `--l2 on` требует
отдельную l2-128 сборку, D-cache toggle требует L2=0. `--l2-probe` требует уже
инициализированный framebuffer и L2 ON. Скрипт сначала переключает кеши, затем
выполняет probe/clock/memory и лишь потом image/bench.

Везде Matrix hashes `c3d5b982 / e5cb9e80`, Loeffler `534b3012 / b3420c10`.
Framebuffer guards=true, stack guard_ok=true; high-water **11020/16384 B**.
Логи target/cache-matrix/l2-zero-{i-only,i-and-d,d-only,all-off,i-and-d-repeat}.log.
37 host tests passed в обеих feature-конфигурациях (2 внешних asset-теста ignored),
обе сборки/clippy -D warnings, fmt, Python syntax, git diff --check прошли.

После экспериментов оставлены **I-cache ON, D-cache OFF, L2=0**. Проверка
после writeback/отключения D-cache: stack guard цел, арена 496 KiB, 160 MHz.
После reset — all-bypass. Это сохраняет прежние предположения остальных DMA-клиентов.
