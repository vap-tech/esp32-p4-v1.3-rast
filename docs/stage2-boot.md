# Загрузка приложения в верхнюю SRAM

Текущая архитектура с 2026-11-08. ESP32-P4 rev1.3, собственный bootstrap и HAL,
без ESP-IDF bootloader, partition table и RTOS; используются небольшие no_std Rust crates.

## Порядок запуска

1. ROM загружает только `.entry` в SPM (8 KiB budget, около 1.4 KiB фактически).
   В bootstrap нет вызовов ещё не загруженного Rust runtime и ссылок на его данные.
2. `src/boot.S` запрещает IRQ, использует прежний нижний boot stack через
   uncached alias, отключает watchdog и autoload. Dirty L1/L2 data сохраняются
   прежней проверенной double-writeback последовательностью с ограничением ожидания.
3. L1/L2 переводятся в bypass, L2MEM — в SRAM (default L2=0, либо l2-128).
   SP возвращается в обычный alias нижнего boot stack.
4. ROM `esp_rom_spiflash_read` (`4fc00158`, local IDF v6.1 ROM header/ld) читает
   manifest и payload. Destination/size обязаны совпадать с символами текущего
   ELF; payload не может пересечь runtime stack. Затем CRC32 всего payload.
5. После `fence rw,rw` / `fence.i` обнуляется только BSS. Rust проверяет cache
   configuration, linker/HAL boundaries и заполняет runtime stack watermark.
   SP переключается на верхний 64-KiB stack, запускается `rust_main`.

Полный destructive SRAM health-test не возвращён: CRC проверяет только новый
загружаемый image, который больше не защищён ROM checksum исходного контейнера.
Свободные арены при старте не сканируются и не перезаписываются.
Ранние boot clock before/after measurements удалены; `--clock` измеряет частоту
по-прежнему во время runtime. IRQ/core1 policy не менялась.

## Карта памяти, default L2=0

| Диапазон | Назначение |
|---|---|
| `30100000..30102000` | SPM bootstrap и резидентные cache-control routines |
| `4ff00000..4ff22000` | Default scoped DMA arena, **136 KiB** |
| `4ff22000..4ff26000` | Default fixed SD/I2S DMA workspaces, **16 KiB** |
| `4ff26000..4ff28000` | Boot stack; после переключения SP — прежний legacy SD CPU scratch |
| `4ff28000..4ff40000` | ROM-reserved 96 KiB, не трогаем |
| `4ff40000.._upper_load_end` | Загружаемые text / rodata / initialized data |
| `_bss_start.._bss_end` | Статические zero-initialized данные, без вынесенных fixed DMA workspaces |
| `_bss_end..4ffb0000` | Динамически вычисляемая work arena, 64-B aligned |
| `4ffb0000..4ffc0000` | Runtime stack 64 KiB с guard/watermark |

На этапе объединённого default image до HTTP: payload около **163 KiB**, BSS **19.25 KiB**,
work около **314 KiB**, отдельно DMA **152 KiB**, то есть около **466 KiB** свободных
арен. После добавления TCP/HTTP work около 265 KiB, DMA по-прежнему 152 KiB;
payload около 189 KiB, BSS около 42 KiB. [Flash-assets и сервер](web.md).
Точные размеры печатает `--memory`. Никакого CODE budget 132 KiB нет:
рост `.upper` уменьшает work arena, а linker запрещает оставлять меньше 32 KiB.
Нижний pool и runtime/statics/work/stack не пересекаются. Для l2-128 верхняя граница
SRAM сдвигается как раньше, размеры work вычисляются заново, а не вычитаются вручную.

Ethernet, audio-lab RX и SD DMA soak используют нижнюю arena; временные CPU
memory-bench buffers — верхнюю. Постоянные I2S/SD/Wi-Fi DMA arrays находятся в `.dma_fixed`
и остаются отдельными владельцами. Наличие DMA pool не делает его автоматически
uncached: прежние handoffs/ограничения кешей остаются, Ethernet требует D OFF/L2=0.
`valid_ahb_range` больше не принимает boot stack или ROM-reserved interval.

Rust не должен оптимизировать совпадение linker aliases как сравнение разных
однобайтовых extern statics. `symbol_address` получает адрес через black_box;
иначе проверка `_internal_start == _bss_end` сворачивалась в ложь до линковки.

## Flash image

`scripts/image.sh` создаёт промежуточный espflash RAM image, затем
`scripts/pack-image.py` проверяет его checksum/SHA и преобразует в окончательный:

| Абсолютный Flash offset | Содержимое |
|---|---|
| `0x2000` | ROM container только с SPM bootstrap, checksum и SHA256 |
| до `0x40000` | Зарезервированный промежуток, FF padding |
| `0x40000` | 64-byte manifest |
| `0x40040` | Runtime payload, точный linked VMA, без relocation во время boot |

Manifest little-endian: magic `P4UPPER1` (8 B), version=1, destination, length,
CRC32, Flash source=40040h, flags=0; остальные 32 B zero. Полином CRC32 reflected
EDB88320h, standard init/final XOR, совпадает с Python zlib.crc32.
Header не выбирает произвольные destination/length: bootstrap сверяет их с ELF.
Packer также записывает ожидаемый CRC32 и его инверсию в специальный slot SPM
bootstrap, затем пересчитывает ROM checksum/SHA. Manifest должен совпасть с этим
значением: смешивание bootstrap и другого payload после неполной прошивки
останавливается до запуска runtime. В исходном ELF slot содержит placeholder;
исполняемый образ создаёт только pack-image.py.
Это integrity check, не подпись или реализация Secure Boot.

Финальный файл `target/firmware.bin` прошивается **целиком по 0x2000** как прежде.
До HTTP его размер был около 411 KiB. С Flash-assets по 0x100000 — около
1 MiB, включая FF gap; это не объём занятой SRAM.
После runtime packer добавляет отдельную gzip-страницу: [формат](web.md).
`target/ram-image.bin` напрямую прошивать нельзя: runtime segment должен быть
прочитан уже после настройки SRAM/L2. Обычный ROM load верхних cache banks в
прошлых экспериментах не сохранял ожидаемое содержимое после старта.

SPM failure path не вызывает Rust: `!H` header/layout, `!R` ROM Flash read,
`!C` payload CRC, `!W` cache writeback timeout, `!T` early trap. Ошибка повторяется
на ввод USB host; display-control.py завершает ожидание с понятной причиной.
При ошибке приложение не запускается, форматирования/автоматического retry нет.

## Упрощение образов

Размерные features `eth-lab` и `sd-format` удалены. В обычном image одновременно
доступны Ethernet, форматирование, JPEG/SD benchmarks и прочие прежние команды.
`audio-lab` только добавляет continuous RX capture diagnostics и не отключает
обычные команды. `l2-128` остаётся экспериментальной аппаратной раскладкой.

## Проверки

- Boot default: приложение действительно исполняется из upper SRAM, cache/layout
  checks и stack guard проходят; SD remount/read CONFIG.TXT совпал.
- Временный образ перенёс **весь Rust runtime**, включая constants и BSS, на
  `4ff80000`: boot и SD read прошли. Это не только тест маленькой ASM function.
  В production origin возвращён на `4ff40000`, тестовых секций/flags не осталось.
- Изменение payload byte остановило boot с `!C`, неверная length — с `!H`.
  Несовпадение manifest CRC с CRC, записанным в bootstrap, также даёт `!H`.
- Объединённый image: DHCP lease и 100 MTU ping (1472 B payload) без потерь/DUP,
  RX errors/missed=0. MAC descriptors действительно в нижнем pool (`4ff00...`).
  После stop arena used=0, peak=19328 B. Guard intact.
- LCD framebuffer: guards и контрольные RGB565 pixels корректны, scanout
  работал одновременно с Ethernet. После старта framebuffer наблюдался RAW
  underrun flag; отсутствие визуальных вспышек этим тестом не доказано.
  Прежняя проблема кратких вспышек при старте/остановке отдельно не исправлялась.
- Короткий I2S tone, digital RX/codec loopback и safe PA stop прошли после
  перемещения static DMA buffers в верхнюю BSS. CPU runtime около 160 MHz.
- Host FAT/media/memory tests, packer tests, frontend queue tests, clippy и сборки
  default/audio-lab/l2-128. Логи и промежуточные образы: ignored `target/stage2/`.

Снятие питания отдельно не проверялось; тестировались download/hard-reset пути.
JPEG/camera качество, длительный audio stress и XIP/PSRAM execution заново не
измерялись. Это loader статически слинкованного runtime, не overlay/unload manager.

Без wifi используются136/16KiB нижних регионов; с wifi build.rs/HAL возвращают
116/36KiB по прежним адресам. [Текущий состав и размеры](build-flavors.md).
