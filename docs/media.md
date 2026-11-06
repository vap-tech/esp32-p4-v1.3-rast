# JPEG и WAV с microSD

Состояние на 2026-11-06: **JPEG с карты работает, WAV с карты пока без звука**.
Работа над воспроизведением остановлена по просьбе пользователя; текущий код
сохранён для продолжения. При загрузке по-прежнему аппаратные полосы и 25%
подсветки. Картинка и музыка запускаются только явными командами.

```sh
python3 scripts/display-control.py --image HOME.JPG
python3 scripts/display-control.py --play SATIE.WAV
python3 scripts/display-control.py --play SATIE.WAV --seconds 12
python3 scripts/display-control.py --stop
```

Без аргумента --image выбирает HOME.JPG, --play — SATIE.WAV. Ctrl-C во время
ожидания проигрывания отправляет ESC. --seconds тоже отправляет ESC по таймеру
хоста. Во время проигрывания прошивка синхронно обслуживает SD/DMA и ESC,
обычные команды/касания/heartbeat ждут окончания операции; LCD DMA независим.
Не следует открывать одновременно монитор и управляющий скрипт на одном порту.
--stop можно вызвать после закрытия предыдущего клиента.

USB-протокол: `:image PATH\n`, `:play PATH\n`, отдельный байт ESC для остановки.
Строка команды ограничена 96 байтами и таймаутом 2 s; пути ASCII 8.3, как в FAT32.
Завершение: MEDIA IMAGE DONE / MEDIA PLAY DONE, результат Ok или Err.
Скрипт возвращает ошибку при Err; для JPEG также проверяет active=true.
Команды читают файлы, записей на SD нет. Отсутствующий файл/неподдерживаемый
формат возвращают ошибку, не подменяются другим файлом.

## JPEG

`src/media/jpeg.rs`: собственный потоковый baseline sequential 8-bit YCbCr
декодер без зависимостей и аллокатора. DQT/DHT, Huffman entropy decode,
restart intervals, integer IDCT, YCbCr → RGB565. Поддерживаются 4:4:4, 4:2:2,
4:2:0; размеры должны совпасть с framebuffer (1024×600), масштабирования нет.
Progressive, arithmetic, CMYK, grayscale и multi-scan не поддерживаются.

Сжатый файл читается буфером 1024 B; MCU переводится в короткие строки RGB565
и пишется в существующий framebuffer PSRAM, без второго полного кадра.
JPEG accelerator/DMA2D пока не используются. При ошибке посреди декодирования
в framebuffer может остаться частичная картинка; активный scanout допускает tearing.
Команда отказывает при активной камере; сама камеру не запускает.

Проверка HOME.JPG (114892 B) на реальной плате:

- MEDIA JPEG: 1024×600, sampling 2×2, **2722232 us** с инициализацией SD.
- MEDIA SCANOUT Ok, MEDIA IMAGE DONE Ok, active=true, guards=true.
- Samples: 13f8 3270 fc0c 93d3 42d0 42f2 2a71 52b5.
- Лог: `target/sd-media-image.log`.

На ноутбуке весь HOME.JPG сравнён с ImageMagick с отключённым fancy upsampling:
максимальная разница 1 шаг каждого канала RGB565; средняя R/G/B
0.02393 / 0.04347 / 0.02781. Постоянные тестовые JPEG/RGB fixtures в tests/data
созданы ImageMagick из собственного градиента; проверяют 444/422/420 и неполные
краевые MCU, уникальную запись каждого пикселя, truncation и malformed input.
Проверка внешнего 1024×600 файла отдельно:

```sh
P4_JPEG_CHECK_FILE=target/sd-media/HOME.JPG \
P4_JPEG_CHECK_OUT=target/sd-media/home.rgb565 \
cargo test --lib --target x86_64-unknown-linux-gnu \
  media::jpeg::tests::decode_external_file -- --ignored
```

## WAV и незавершённое кольцо DMA

`src/media/wav.rs`: RIFF/WAVE PCM16 mono/stereo, chunks с проверкой границ,
неизвестные chunks и odd padding. Проверяются rate/byte rate/block align,
моно дублируется в оба I²S слота; хвост буфера дополняется нулями.
Проигрывание принимает только 16000 Hz, прочие частоты отказывает до включения PA.
SATIE48.WAV (48000 Hz) пока не проигрывается.

`hal::i2s::Tx::new_stream` подготавливает два SRAM-буфера по 512 stereo frames
(2048 B каждый), по 32 ms при 16 kHz, с циклическими 64-byte-aligned descriptors.
AHB TX0: owner check и auto writeback; предполагается ожидание освобождения
буфера и возврат OWN после refill. Кодек настраивается при циклической тишине,
затем DMA переинициализируется для файла. DAC остаётся на безопасном -24 dB,
PA=GPIO11. При ошибке предпринимаются mute, PA low, остановка DMA и clock off.

**На железе пока ошибка до первого refill**, звук пользователем не подтверждён.
Последний лог `target/sd-media-audio-uncached.log`:

- rate=16000, channels=1, data=5874834 B; подготовлено 1024 frames.
- index=0, refills=0, DMA raw=5 (done + descriptor error).
- TX config=60 (0x3c), link=8 (park), current descriptor=0x4ff22ac0.
- После cleanup PA=false, raw=7; команда возвращает Err(Audio(I2s(Dma(5)))).

Предыдущий вариант читал OWN через invalidate L1; последняя проба использует
uncached SRAM alias +0x40000000 для опроса/публикации descriptors, но ошибка
осталась. Диагностическое Status.owners пока читает обычный cached alias,
поэтому это поле нельзя считать надёжным live-снимком OWN. Корневая причина
ещё не установлена: продолжить с проверкой перехода/owner/writeback и порядка
публикации двух descriptors. Не считать playback рабочим по прохождению host-тестов.

Для совместного доступа SD/I²S к одному таймеру File над SdDevice предоставляет
with_timer: кратковременное эксклюзивное заимствование timer транспорта, без
одновременного файлового I/O. Дополнительные аппаратные Timer не создаются.

## Память и проверка на этапе bring-up медиа

После этого этапа карта памяти расширена, кеши отключены, стек вынесен отдельно:
актуальные данные — [memory.md](memory.md). Числа ниже описывают предыдущую пробу.

Новые крупные обработчики и старые диагностики помечены inline(never), чтобы
компилятор не складывал их временные буферы в постоянный stack frame rust_main.
До разделения его frame занимал около 8 KiB; после — 592 B.
BSS текущего образа — 0x5c80 B в 32 KiB DATA, остаётся 9088 B под стек.
Карта памяти не расширялась, ROM-области не затрагивались. Полного runtime
измерения максимальной глубины стека пока нет.

Образ этой пробы: **114976 B**. 31 обычный host-тест прошёл,
проверка полного HOME.JPG через отдельный ignored test тоже прошла.
Файлы медиа, дампы и аппаратные логи остаются в ignored target/ и на карте.


## JPEG A/B: стоимость записи framebuffer

Явная команда, кеши остаются выключенными:

```sh
python3 scripts/display-control.py --image HOME.JPG --jpeg-bench HOME.JPG
# Когда framebuffer уже активен:
python3 scripts/display-control.py --jpeg-bench HOME.JPG
```

USB `:jpeg-bench PATH` с newline; требуется активный framebuffer и выключенная
камера. По умолчанию ничего не запускается. Отсутствующий файл/ошибка декодера,
разные hashes/числа пикселей или повреждение guards дают Err и ненулевой exit
скрипта. Файлы SD не изменяются.

Порядок **A–B–B–A**. Каждый проход заново инициализирует SD, монтирует FAT32 и
открывает тот же файл; кэши файловой системы тоже новые. Decode timer начинается
после open и охватывает одинаковые SD reads, JPEG header/Huffman/IDCT/YCbCr,
контрольную сумму всех RGB565 samples и callback вывода. total_us дополнительно
включает подготовку SD/mount/open. USB logging, framebuffer checksum и проверки
guards находятся за пределами timed decode.

A пишет исходным Framebuffer::write_run (volatile u16); B только считает checksum.
Оба проходят **одну instantiation JPEG decoder и один callback**, флаг записи
выбирается во время выполнения, без отдельной специализированной B-версии.
Checksum FNV по u16 считается в обоих режимах в одинаковом порядке MCU rows;
в конце выводится, поэтому RGB/IDCT вычисления не могут быть выброшены.
Полный checksum framebuffer до/после каждого прохода проверяет, что B не
изменяет картинку, а повторные A дают тот же результат.

sd_us — время внутри read операций SdDevice во время decode; файловая логика,
копирование из FAT caches и вызовы декодера в него не включены. sd_bytes считает
физическое sector I/O, поэтому немного больше размера JPEG, включает FAT/edges.
Это диагностика реального SD вклада, не отдельный storage benchmark.

Разность средних A и B оценивает стоимость существующего вывода на этом файле
при непрерывном LCD scanout; она не является гарантированным выигрышем GDMA.
B тоже тратит время на checksum и ветвление. Полосы SRAM, u32 stores и GDMA в этом
эксперименте не реализуются; формат JPEG и кеши не переключаются.


Проверка на ESP32-P4 rev1.3 с HOME.JPG (114892 B), LCD active, L1/L2 bypass:

| Проход | Режим | Decode, us | Total, us | SD I/O, us |
| --- | --- | ---: | ---: | ---: |
| 1 | A, u16 + hash | 7435106 | 7465099 | 48207 |
| 2 | B, hash only | 7260751 | 7290704 | 48273 |
| 3 | B, hash only | 7260675 | 7290631 | 48210 |
| 4 | A, u16 + hash | 7435182 | 7465137 | 48226 |

Средние decode: **A=7435144 us, B=7260713 us**. Разность **174431 us,
2.346% от A**. Физическое чтение во всех проходах: 114 calls, 115712 B;
SD I/O около 48.23 ms (0.65% A). Блоки чтения и полный decoder одинаковы.
Checksum pixel stream **c3d5b982**, 614400 pixels, совпали во всех проходах.
Framebuffer hash **e5cb9e80** до/после, guards=true; B ничего не изменил.
Оба hashes независимо совпали с ранее полученным host decode HOME.JPG:
первый в MCU-row order, второй по raster u32 words. Это не один и тот же hash format.
Стек high_water=10076/16384 B, guard цела. Камера не включалась.

На этом изображении текущие мелкие записи PSRAM дают лишь небольшую часть общего
времени. Основной остаток приходится на CPU processing/файловую логику, включая
выполнение decoder без I/D caches. Этот A/B **не разделяет** Huffman, IDCT,
цветовую конверсию и общий checksum; для выбора оптимизации нужен следующий
профиль по стадиям. 174 ms нельзя обещать как выигрыш конкретного CPU/DMA copy.

Лог: target/jpeg-abba-no-cache.log; проверка состояния target/jpeg-abba-status.log.
Отсутствующий MISSING.JPG корректно возвращает Err(Io(NotFound)), script exit!=0,
никакой подмены/записи изображения не происходит. Образ 125376 B прошит;
33 host tests, fmt, clippy и Python syntax check проходят. Логи не входят в Git.

## Профиль стадий JPEG без изменения кешей

```sh
python3 scripts/display-control.py --image HOME.JPG --jpeg-profile HOME.JPG
# Если framebuffer уже активен:
python3 scripts/display-control.py --jpeg-profile HOME.JPG
```

USB `:jpeg-profile PATH` + newline. Аппаратная команда сейчас проверяет ожидаемые
счётчики для 1024×600 / 4:2:0, как у HOME.JPG; для другого sampling проверку
счётчиков нужно обобщить. LCD scanout остаётся активным, камера выключена;
L1/L2 bypass и L2 capacity=0 сохраняются. Никакого автоматического запуска профиля
нет. Формулы Huffman/IDCT/color, SD reader и способ u16-записи не оптимизировались.

Profile опционален; P и U используют ту же decoder instantiation/callback,
включение счётчика выбирается runtime. Последовательность **P–U–U–P**:
P — измерения стадий, U — контроль без чтений stage clock. В обоих режимах
считаются checksum и выполняются framebuffer writes. Готовые RGB565 rows одного
MCU временно собираются в **512 B SRAM на стеке**, затем выдаются прежними короткими
строками: так color/output измеряются раз за MCU, а не за pixel/row. Это одинаково
для P и U. Из-за staging, дополнительных проверок и изменения машинного кода
контроль этой сборки нельзя смешивать с предыдущими 7.435144 s как с идентичным
бинарником. Ни полосовых DMA-буферов, ни новой внешней памяти не добавлялось.

Стадии:

- Entropy + dequant: bitreader, Huffman, DC/AC, zigzag и умножение на quantizers,
  одним интервалом за block 8×8. Dequant оставлен внутри распаковки, как раньше.
- IDCT: прежняя integer 8×8 функция, один интервал за block, включая DC-only shortcut.
- Color: nearest chroma sampling и RGB565, один интервал за MCU.
- Output + checksum: общий checksum всех RGB565 и прежние u16 stores, интервал за MCU.
- Header: разбор заголовка и таблиц, один интервал на изображение.

Копирование 8×8 block samples в component planes, restart/MCU setup, loop control,
часть bookkeeping/clock overhead находятся в **residual**, не приписываются IDCT
или цвету. SD I/O входит в header/entropy intervals и отдельно показано через
SdDevice stats; повторно прибавлять его к decode wall time нельзя.

Clock — 64-bit HP mcycle, coherent high/low/high read. Он калибруется по SYSTIMER
в окне 20 ms до/после профиля: **159993100 / 159985602 Hz**. Это измеренная частота
счётчика около 160 MHz, а не предположение о максимальной частоте SoC. Minimum
back-to-back clock-pair delta=16 cycles. Коэффициент переводит cycles в us;
индивидуальные интервалы не корректируются вычитанием overhead. Во всём изображении
14592 blocks, 2432 MCU; stage clock не читается для каждого coefficient/pixel.

Результат HOME.JPG, 1024×600:

| Проход | Режим | Decode us | Total us | SD I/O us |
| --- | --- | ---: | ---: | ---: |
| 1 | P | 7561674 | 7591628 | 48338 |
| 2 | U | 7530255 | 7560208 | 48298 |
| 3 | U | 7530352 | 7560307 | 48262 |
| 4 | P | 7561683 | 7591650 | 48327 |

P mean=**7561678 us**, U mean=**7530303 us**. Измеренная добавка
инструментирования **31375 us / 0.417%**. Средние двух P-проходов:

| Стадия | Время us | Доля P | Вызовов | Примерно cycles/call |
| --- | ---: | ---: | ---: | ---: |
| Entropy + dequant | 1436205 | 18.99% | 14592 | 15747 |
| IDCT | **4440107** | **58.72%** | 14592 | 48683 |
| Color | 995984 | 13.17% | 2432 | 65522 |
| Output + checksum | 331688 | 4.39% | 2432 | 21820 |
| Header | 1402 | 0.02% | 1 | ~224285 |
| Residual/control/assembly | 356294 | 4.71% | — | — |

Таким образом IDCT — крупнейший измеренный участок текущего software decoder.
Это ещё не сравнение кешей и не доказательство единственной причины замедления:
у всех этих инструкций/данных доступ к SRAM без L1. Нужен отдельный эксперимент,
если решим менять IDCT или cache policy. Output включает checksum, поэтому 332 ms
не заменяет A/B-оценку 174 ms собственно добавки framebuffer stores.

Проверки: во всех проходах 614400 output pixels, stream hash **c3d5b982**,
framebuffer hash **e5cb9e80**, guards=true. Счётчики P точно [14592,14592,2432,2432,1];
host-test также проверяет профилированные/обычные pixels, counts и фальшивый clock
на небольшом JPEG. Стек high_water=**10988 / 16384 B**, нижняя зона цела.
34 обычных host tests, fmt/clippy/Python syntax проходят; образ **129872 B** прошит.

### Откуда исполняется код

Проверены ELF symbol table и disassembly образа профиля 129872 B:

- Decoder::block, включающий inlined IDCT: **4FF00164**, size=1320 B.
- Reader::symbol (Huffman): **4FF0949A**, size=244 B.
- decode_profiled для измерительных проходов, включая inlined color: **4FF01496**.
- decode_profiled обычного image command: **4FF01BFE**.
- BASIS matrix, по адресу из LUI/ADDI в IDCT: **4FF1F09C**.
- cycle_count: **4FF18F6C**, три CSR reads и rollover retry, без вызовов ROM.

Весь hot decoder и его таблицы находятся во **внутренней SRAM**, не в mapped flash
или PSRAM. В IDCT видны MUL/MULH, прямого ROM decoder/IDCT вызова нет. Вспомогательные
memcpy/memset тоже из нашего SRAM image. Три ROM MSPI helper по-прежнему нужны
только при PSRAM initialization, вне timed JPEG decode.

Логи: target/jpeg-stages-no-cache.log, target/jpeg-stages-status.log,
target/jpeg-profile-symbols.txt, target/jpeg-profile-disassembly.txt,
target/jpeg-profile-host-tests.log. Они остаются локальными, ignored.

## Аудит CPU clock и арифметики IDCT

Проверка 2026-11-06: частота/PLL/делители не изменялись. Добавлена команда
`python3 scripts/display-control.py --clock`, USB `:clock` + newline. Она читает
LP_CLKRST_HP_CLK_CTRL, HP_SYS_CLKRST_ROOT_CLK_CTRL0/1/2 и калибрует mcycle по
SYSTIMER за 20 ms. До/после cache bypass измерения дополнительно делаются в
startup; два результата сохранены в отдельной **16 B NOLOAD .boot_audit**,
которая не обнуляется вместе с BSS. --clock печатает их даже если ранний USB
boot log не попал к хосту. Нового heap, изменения CPU PLL или делителей нет.
Boot SYSTIMER используется только для измерения, runtime затем создаёт свою эпоху.

Readback в обеих конфигурациях идентичен:

- HP root register **1FFFFFFDh**, source bits=1: **CPLL**.
- ROOT_CLK_CTRL0=**00000020h**: integer CPU divider=**2**, numerator=0,
  denominator=0 (дробная часть не используется).
- ROOT_CLK_CTRL1=**00000000h**, ROOT_CLK_CTRL2=**00010000h**.
- mcycle около **160 MHz**, следовательно upstream CPLL около **320 MHz**
  (вывод из измерения и делителя, не отдельное чтение аналоговых PLL-регистров).

| Измерение | mcycle Hz | L1 bypass | L2 bypass | L2 capacity |
| --- | ---: | --- | --- | --- |
| Текущий startup до bypass | 159995000 | исходное ROM cache state | исходное ROM cache state | 256 KiB |
| Тот же startup после bypass | 159997150 | 13h | 20h | 0 |
| Текущий runtime | 159995100 | 13h | 20h | 0 |
| Кешируемый reference, до JPEG 1 | 159987101 | 0 | 0 | 256 KiB |
| Кешируемый reference, до JPEG 2 | 159991750 | 0 | 0 | 256 KiB |
| Кешируемый reference, после JPEG | 159991550 | 0 | 0 | 256 KiB |

Для независимого повторения взят **git archive 9288004** в ignored
`target/clock-reference/`: исходная кешируемая версия, прежний linker/stack,
без новых арен, bypass startup и stage profiling. Добавлены только read-only
clock audit и cycle counter. Диагностическая сборка **115760 B** временно прошита
и запущена на той же плате/карте. HOME.JPG: **2521673 / 2615403 us**, обычный
software decoder, sample pixels совпали, framebuffer guards=true. Первый decode
до включения PSRAM scanout, второй при активном LCD. Исторический результат
2.722232 s не имел записи CPU clock; его точную прошлую частоту невозможно
извлечь из старого лога. Но повторение сохранённого исходного кода и прямой
before/after bypass audit подтверждают **160 MHz в обеих проверенных конфигурациях**.
Изменение кода/раскладки из-за диагностики влияет на cache performance, поэтому
2.52–2.62 s не выдаются за побитовое воспроизведение прежнего бинарника.
Оснований объяснять разницу 2.72/7.43 переходом 360→160 MHz не найдено.

После проверки **возвращена текущая прошивка без кешей**, 130800 B. Проверены
cache flags, нулевой L2 capacity, --clock, JPEG (7442169 us) и LCD/guards;
камера/аудио не запускались. В рабочем дереве историческая версия ничего не
заменила; временные reference sources и firmware остались только в target/.

### Что реально делает IDCT

Арифметика целиком **integer fixed point**:

- coefficients и промежуточный массив — i32; произведения/суммы — i64;
- BASIS 8×8 — заранее вычисленные целые Q14, в rodata SRAM;
- два разделяемых прохода 8×8, каждый с суммированием по 8 коэффициентам;
- **1024 multiply-accumulate terms на полный блок** (2×8×8×8), не прямой
  четырёхцикловый 2D IDCT, но и не факторизованный AAN/Loeffler;
- DC-only shortcut уже есть: если 63 AC=0, заполнение константой;
- частичных row/column-zero shortcuts пока нет;
- округления/деления — shifts, runtime sin/cos и вычисления BASIS отсутствуют.

Повторно проверен Decoder::block, куда inlined IDCT встроен компилятором:
**4FF00164**, size=1320 B, SRAM. В циклах видны **MUL + MULH**, ADD и SLTU
для 64-bit sums/carries, сдвиги. Внутри блока **нет floating-point инструкций,
вызовов __muldf3/__adddf3/__divdf3, __muldi3, sin/cos**. Вспомогательные
memcpy/memset вызываются из SRAM. Таким образом f32/f64/software floating point
не являются причиной этих 4.44 s. Но широкие integer MAC на 32-bit CPU и
матричный алгоритм дают много инструкций, исполняемых сейчас без I-cache.
Это основание исследовать факторизацию/нулевые строки, не обещание конкретного
ускорения и не разрешение просто сузить суммы до i32 без доказательства границ.
IDCT в этом заходе не оптимизировался, округление/пиксели не менялись.

Логи: target/clock-cached-reference-{first,second,after}.log,
target/clock-audit-restored.log, target/clock-audit-restored-after.log,
target/idct-arithmetic-audit.txt, target/clock-audit-disassembly.txt.
34 host tests, fmt/clippy/Python syntax и git diff --check проходят.


Следующий эксперимент: [отдельный Loeffler IDCT](idct.md), матрица сохранена,
ошибки до цвета/в RGB565 измерены, сравнение L–M–M–L на плате выполнено.
Матрица остаётся обычным default; --jpeg-idct-bench выбирает новый вариант явно.
