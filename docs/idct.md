# Loeffler IDCT: отдельная реализация и сравнение с матричным эталоном

> Программный JPEG теперь optional: feature software-jpeg; benchmarks дополнительно diagnostics.
> [Состав сборок](build-flavors.md). Код и результаты ниже сохранены.

Проверено на ESP32-P4 rev1.3, около 160 MHz, **все кеши обходятся, L2 size=0**,
PSRAM x16 DDR 80 MHz, LCD 1024×600 active. Кеши, частота, DMA и периферия в этом
эксперименте не менялись. Entropy/dequant и RGB565 conversion сохранены.

## Реализации и API

- `src/media/idct.rs`: исходная Q14 матрица сохранена как `matrix`; оба прохода
  и округления прежние. Общий DC-only shortcut перенесён без изменения результата.
- `src/media/loeffler.rs`: Rust-порт **8-bit 8×8 _jpeg_idct_islow** из libjpeg-turbo
  src/jidctint.c, alternate Loeffler–Ligtenberg–Moschytz, 12 multiply 1D kernel,
  CONST_BITS=13, PASS1_BITS=2, column pass + row pass, zero-column/zero-row shortcuts.
- Деквантование по-прежнему выполняется в entropy decoder; порт получает готовые
  dequantized i32 coefficients. i64 products/sums сохранены для безопасных границ.
- Вместо libjpeg runtime range-limit table используется явное насыщение 0…255;
  отрицательные значения DESCALE округляются арифметическим сдвигом как в IJG.
- Матрица остаётся **обычным default** для `jpeg::decode` и команды --image.
  Новый алгоритм выбирается явно, математический эталон не удалён.

Внешний код может выбрать Loeffler без включения профилирования:

```rust
use esp32_p4::media::{idct::Kind, jpeg};
let mut options = jpeg::Profile::disabled().with_idct(Kind::Loeffler);
jpeg::decode_profiled(&mut file, 1024, 600, |x, y, pixels| {
    framebuffer.write_run(x, y, pixels);
}, &mut options)?;
```

Profile с clock считает blocks и dc_only_blocks. Никакого runtime heap/внешних
Cargo dependencies нет. Нормальные команды по-прежнему не запускаются автоматически.

## Проверка на ноутбуке

Pinned libjpeg-turbo commit **29e4d9b0178f08158fc72c6068f086c5ba27b772**:
[исходный jidctint.c](https://github.com/libjpeg-turbo/libjpeg-turbo/blob/29e4d9b0178f08158fc72c6068f086c5ba27b772/src/jidctint.c).
SHA256 исходника **4ba1bd508d16b3b0d5849a7e7b9dcab79c24cd1701edb054605ef47f5ef8f239**.

Весь HOME.JPG декодирован на host. С каждого блока до цветовой конверсии сохранены
коэффициенты и результаты обоих IDCT. Всего **14592 blocks / 933888 component samples**.

| Метрика Matrix → Loeffler | Результат |
| --- | ---: |
| DC-only blocks | **264 / 14592 = 1.81%** |
| Блоки с хотя бы одним изменением | 13662 |
| Изменённые component samples | 85061 / 933888 |
| Максимальная абсолютная ошибка sample | **1 / 255** |
| Средняя абсолютная ошибка sample | около 0.0911 |
| Изменённые итоговые RGB565 pixels | 51948 / 614400, около 8.46% |
| Максимальная разница каналов RGB565 | **1 / 1 / 1 steps R5/G6/B5** |

Побитового совпадения с матрицей нет и оно не объявляется требованием: порядок
проходов/фиксированные константы/округления различаются. Указанная граница ошибки
измерена для этого файла, а не доказана для всех произвольных JPEG. 444/422/420 и
неполные MCU края дополнительно проверены на небольших независимых JPEG/RGB fixtures.

### Независимый C oracle

```sh
export PATH="$HOME/.cargo/bin:$PATH"
python3 scripts/check-idct-reference.py --jpeg target/sd-media/HOME.JPG
```

Host-only Python script загружает **проверенный SHA256 pinned источник** в ignored
`target/idct-reference`, извлекает 8×8 routine, компилирует системным cc с UBSan,
запускает ignored Rust comparison test и сравнивает каждый sample.

C oracle использует оригинальный порядок C-выражений, но с теми же адаптациями,
которые сделаны для Rust: вход уже dequantized i32, широкие JLONG products/sums,
явный clip вместо masked range table, safe multiplication вместо C negative left
shift. Это проверка порта факторизации, не заявление о побитовом совпадении с
немодифицированным libjpeg на синтаксически допустимых, но непохожих на фото данных.

**Все 933888 samples Rust Loeffler / C oracle совпали побитно**, UBSan clean.
Ещё **1156 stress blocks**: единичные коэффициенты на границах, все ±522240,
чередующиеся знаки и random vectors — тоже совпали с C и без UB/overflow.
C-код/компилятор не участвуют в сборке или исполнении прошивки; Cargo deps пусты.

### Границы арифметики

Вход текущего baseline parser: DC -2048…2047, AC signed до 10 bits, quant 1…255.
Поэтому общий консервативный предел dequantized abs coefficient **522240**
(для AC фактически меньше). Интервальная оценка сети kernel даёт не более
**178219 × max_input** до DESCALE. В первом проходе после округления/сдвига 11:
abs workspace <= **45445846**, что помещается в i32. Максимальная сумма входов
для i32 pre-multiply butterfly во втором проходе — четыре workspace values,
менее **181783384**, тоже помещается в i32. Продукты и прочие суммы вычисляются
в i64; консервативный raw second-pass output около **8.1e12**, далеко от i64 limit.
Такой запас не основан на том, что HOME.JPG имеет небольшие коэффициенты.
Debug assertions и debug-overflow checks дополнительно проходят stress suite.

## Измерение на плате

```sh
python3 scripts/display-control.py --image HOME.JPG --jpeg-idct-bench HOME.JPG
# Если framebuffer уже активен:
python3 scripts/display-control.py --jpeg-idct-bench HOME.JPG
```

USB `:jpeg-idct-bench PATH` + newline. Диагностика ожидает 1024×600 / 4:2:0,
как HOME.JPG; камера должна быть выключена. Порядок **L–M–M–L**:
L=Loeffler, M=matrix. Обе реализации в одном SRAM image, одинаковые SD reads,
clock/profile callback, LCD, dequant, color и output+checksum. Сравнение повторных
проходов ведётся отдельно для каждого алгоритма, так как их hashes различаются.

| Проход | IDCT | Decode us | IDCT us | Entropy us | Color us | Output us | SD us |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | L | 4844576 | 1738190 | 1425101 | 995298 | 329971 | 48226 |
| 2 | M | 7354700 | 4245808 | 1423538 | 995000 | 331734 | 48270 |
| 3 | M | 7354406 | 4245800 | 1423446 | 994993 | 331903 | 48153 |
| 4 | L | 4844833 | 1738179 | 1425147 | 995293 | 330203 | 48280 |

Средние **Matrix 7.354553 s → Loeffler 4.844704 s**, ускорение полного decode
**1.518×**, сокращение времени **34.13% / 2.510 s**. Сам IDCT:
**4.245804 → 1.738184 s**, ускорение **2.442×**. Это честная пара внутри одной
сборки; предыдущие 4.44 s из другого layout/codegen не подставляются вместо
контрольного Matrix. Между проходами частота/память не переключались.
Небольшое отличие среднего wall time между сборками не выдаётся за эффект алгоритма.

DC-only=264 одинаков в обоих алгоритмах. Поэтому 14.94 млн MAC матрицы были
верхней оценкой; после общего DC-only shortcut полный matrix path выполняется
для 14328 blocks, около **14.67 млн multiply-accumulate terms**. У Loeffler
дополнительно работают zero-column/row shortcuts, поэтому 192 multiplies/block
тоже верхняя оценка, не измеренный фактический instruction count.

### Корректность и состояние после замера

| Алгоритм | Stream MCU-order hash | Framebuffer raster-u32 hash |
| --- | --- | --- |
| Matrix | c3d5b982 | e5cb9e80 |
| Loeffler | 534b3012 | b3420c10 |

Каждый из **обоих** hardware hashes совпал с соответствующим полным host reference.
Повторы внутри алгоритма совпадают, guards=true, LCD active, camera=false.
Stack peak **10812 / 16384 B**, guard цела. Runtime clock audit ~159993200 Hz,
CPU divider=2, L1 bypass=13h, L2 bypass=20h, L2 size=0.

Последний L оставлен на дисплее. Обычная --image снова покажет матричный эталон.
При reset по-прежнему аппаратные полосы; auto JPEG/music не добавлялись.
Аудио/камера не запускались, файлов SD не записывали.

Обе горячие функции остаются в SRAM: Loeffler transform **4FF184E6**,
kernel **4FF188D0**, matrix_dense **4FF1873C**. Для размещения дополнительного
кода без изменения linker regions не входящие в timed decode console handlers
jpeg_bench/jpeg_profile/jpeg_idct_bench помещены в уже зарезервированную SPM
секцию. **Ни IDCT, ни entropy/color в SPM не переносились.** Кеши, частота,
SRAM/PSRAM arenas и расположение framebuffer не менялись.

Образ **134848 B** прошит; последующая сборка после host-only test additions
побитно совпала с прошитым bin, SHA256
**de3de631e24b1e40f76adacc80a94c34267c8809f152ca3d16298fd960d7b04b**.
37 обычных host tests проходят, две проверки с внешним файлом остаются ignored;
comparison с HOME.JPG и C oracle выполнен явно. fmt/clippy/Python syntax проходят.

Логи/сравнения остаются в ignored target/idct-reference/: host-comparison.log,
c-comparison-final.log, board-comparison.log, board-status.log, firmware-symbols.txt,
коэффициенты и RGB565 references. Исходник C скачивается только как host reference.

## Attribution

This software is based in part on the work of the Independent JPEG Group.
Порт сохраняет copyright notices. Лицензии и описание изменений:
[licenses/README.md](../licenses/README.md),
[полный README.ijg](../licenses/libjpeg-turbo-README.ijg.txt),
[libjpeg-turbo LICENSE](../licenses/libjpeg-turbo-LICENSE.md).
