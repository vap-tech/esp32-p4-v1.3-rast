# ESP32-P4 rev1.3: PIE 2.1 и HWLOOP — 2026-11-15

Это первый проверенный набор SIMD-ядер, не полное покрытие ISA и не реализация
моделей/майнера. Штатная сборка и DMA-политика не меняются: I0/I1 ON, D OFF,
L2=0; `pie-lab` включается отдельно. HP0 продолжает владеть периферией.

## Что именно ускоряется

PIE — собственное расширение Espressif, не RVV и не свидетельство профиля RV23.
Восемь Q-регистров по128 бит содержат16×int8,8×int16 или4×int32.
Есть QACC_H/QACC_L по256 бит, скалярный XACC40, аппаратные циклы,
целочисленная арифметика/сдвиги/перестановки и fused ALU+memory операции.
Это не универсальный128-битный процессор и не SIMD float32.

Espressif уже использует эти инструкции в esp-dsp/esp-dl. Отсутствие обычного
Rust intrinsic или высокоуровневого IDF API не означает, что ускоритель недоступен.
Копирование/DSP — только первая часть возможностей; FFT, QACC и перестановки
нужно исследовать отдельно.

## Первичные источники и несовпадение версий

* [TRM именно rev1.3](https://documentation.espressif.com/esp32-p4-chip-revision-v1.3_technical_reference_manual_en.pdf):
  §1.7.1 — HWLOOP, §1.7.2 — PIE, §7.3.1 — пути доступа к памяти.
  §1.7.2 оставляет полный справочник инструкций как **to be added later**.
* [Текущий TRM](https://documentation.espressif.com/esp32-p4_technical_reference_manual_en.pdf):
  глава4 содержит подробный ISA, но §2.7.2 прямо указывает **PIE V2.2.0**.
  Новые операнды `sat`/`rm`, кодировки и описания нельзя автоматически переносить
  на наш ранний чип. Даже разделы нового руководства местами требуют проверки
  по фактическому результату.
* [Espressif: введение в PIE](https://developer.espressif.com/blog/2024/12/pie-introduction/):
  load/store128, saturating add16 и fused data path. Их benchmark360MHz/L2=128KiB
  не является нашим результатом. Пример обычного `x+y` не эквивалентен насыщению
  при переполнении; наш эталон использует `saturating_add`.
* ESP-IDF v6.1, commit `fff9895c82d744c7237be8847347bdd1b07c6643`, локально
  `/home/kot/esp-idf`. Особенно:
  `components/riscv/include/riscv/csr_pie.h`, `csr_hwlp.h`, `rvruntime-frames.h`,
  `components/soc/esp32p4/include/soc/soc_caps.h`,
  [decoder fixture со старыми мнемониками](https://github.com/espressif/esp-idf/blob/fff9895c82d744c7237be8847347bdd1b07c6643/components/esp_gdbstub/test_gdbstub_host/rv_decode/xesppie.S).
  Из fixture составлен [инвентарь360 мнемоник](pie-inventory.tsv):26 сейчас
  выполняются в наших тестовых ядрах. Это не утверждение, что все варианты
  операндов этих26 или все360 инструкций проверены на железе.
* [Официальный esp-dsp](https://github.com/espressif/esp-dsp), в частности
  `modules/dotprod/fixed/dsps_dotprod_s16_arp4.S`: источник для изучения,
  библиотека в прошивку не подключена.

PDF и извлечённый текст сохранены в игнорируемом `reference/espressif-p4/`.

## Как собирается без чужого runtime

Stock LLVM23.1.3 отвергает мнемоники `esp.*`. Проверены кодировки фирменным
assembler **PIE2.1**, crosstool-NG `esp-15.2.0_20251204`, binutils2.45:

```sh
python3 scripts/pie-opcodes.py /path/to/as-xespv2p1 /path/to/objdump-xespv2p1
# После осознанного изменения kernels.S:
python3 scripts/pie-opcodes.py /path/to/as-xespv2p1 /path/to/objdump-xespv2p1 --write
```

Флаг assembler: `-march=rv32imafc_xespv2p1_xesploop -mno-relax`.
`src/hal/pie/kernels.S` — читаемые исходники;
`encoded.S` —85 сверенных вхождений инструкций через `.word`, потребляемых
обычным Rust `global_asm!`. Cargo не запускает внешний assembler, не линкует
C/IDF и не скачивает toolchain. HWLOOP собирается без compressed/relaxation,
чтобы перенесённый относительный offset совпадал с окончательным кодом.

Хостовый архив был скачан из tools.json IDF и проверен по размеру/SHA-256;
временные бинарники лежат в `/tmp/p4-pie-tools/`, manifest сохранён в reference/.
Вызывать современную wrapper-команду `riscv32-esp-elf-as` без выбора версии
опасно для воспроизводимости: по умолчанию она выбирает2.2.

## Владение и память

`hal::pie::Pie` выдаётся через unsafe `claim_hp1()` только на hart1, когда
PIE/HWLOOP выключены. Это один эксклюзивный владелец, `!Send + !Sync`.
Инициализируются Q0..Q7, QACC, XACC и CFG. CSR0x7f2 проходит
INITIAL→CLEAN; после инструкций аппаратно становится DIRTY, Drop выключает PIE.
HWLOOP probe отдельно включает и выключает CSR0x7f1.

Обработчик HP1 IRQ18 использует только t0/t1/t2; он не трогает PIE/HWLOOP/FP.
Поэтому для этого фиксированного worker полный context switch не нужен.
Это **не** готовый ABI для нескольких задач, миграции между ядрами или ISR,
который сам использует SIMD. Для такого режима нужны сохранение/восстановление
Q/QACC/XACC/SAR/UA/config и hardware-loop регистров. Один CSR state не сохраняет
их содержимое. Erratum HWLOOP из IDF также остаётся актуальным.

API принимает выровненные `Block([u8;16])`, одинаковые длины и непересекающиеся
mutable/shared slices. Обрабатывает0 блоков без чтений, не округляет длину вверх.
Для int16 dot product ограничивает длину504 сэмплами, чтобы полный диапазон
int16 гарантированно помещался в signed XACC40. Большие сигналы можно считать
порциями с последующим i64 суммированием на CPU.

**Найденное отличие адресных путей:** первая проба VST128 через `0x8ff…`
остановила HP1: `mcause=0x38000005`, `mepc=0x4ff40148` (VST128),
`mtval=0x8ff8b0d0`. HP0/web продолжили работать. Код исключения5 в TRM обозначен
PMP load access fault, хотя faulting instruction — store; точную внутреннюю
причину пока не установили. Не называем это доказанным общим erratum чипа.

После перехода scratch на обычный SRAM-путь `0x4ff…` при D-cache OFF тесты
проходят. TRM §7.3.1 отдельно описывает128-битный DBUS и прямой `0x8…` путь,
обычно предназначенный для отладки. HAL пока явно отвергает `0x8ff…`, PSRAM,
невыравненные адреса и выход за SRAM. Scalar-доступ через `0x8ff…` по-прежнему
работает для очередей/status/стека; это не доказательство пригодности того же
пути для128-битного доступа.

Scratch принадлежит только HP1, не участвует в DMA. Перед тестом проверяется
D-cache OFF/L2=0. Смена кешей разрешается только после завершения работы и
штатного подтверждения SPM-паузы. Во время probe HP1 не обслуживает xorshift
queue; HP0 и LP продолжают работу. Не ждать от pause немедленного прерывания
вычислений: worker завершает текущую задачу перед подтверждением.

## Что проверено

* Copy128 и signed saturating add8/add16/add32; signed sub16, min16/max16.
* AND/OR/XOR/NOT128.
* Lane-wise left/logical-right/arithmetic-right32 при каждом shift0..31.
* Signed int8/int16 dot product, отрицательные результаты, старшие8 бит XACC.
* Fused int16 MAC + загрузка следующего вектора; последний блок обрабатывается
  отдельно без чтения за концом. Длины0/1/7/63.
* HWLOOP на1/2/3/7/31/1000 итераций. Тело содержит минимум **шесть32-битных
  инструкций** по TRM §1.7.1.5; проверяется фактическое число сложений.
* Крайние signed значения, переполнения, детерминированный random, длины0..63,
  canary до/после output и неизменность хвоста после коротких ALU-операций.
* INITIAL/CLEAN/DIRTY/OFF, повторное получение token, IRQ во время вычислений,
  дальнейшая работа worker и pause/resume.

Индексы `errors`:0copy,1add8,2add16,3add32,4sub16,5min16,6max16,
7and,8or,9xor,10not,11dot16,12HWLOOP,13shl32,14shr32,15sar32,16dot8,17dot16-fused.
Счётчик `checked` смешивает сравнения блоков и отдельных результатов; это не
количество уникальных инструкций/всех возможных входов.

## Запуск

```sh
./scripts/image.sh --features pie-lab
espflash write-bin --port /dev/ttyACM0 0x2000 target/firmware.bin
python3 scripts/display-control.py --worker 'pie run'
python3 scripts/display-control.py --worker 'pie status'
```

В консоли, в том числе при работающем web:
`:system worker pie run`, `:system worker pie status`.
Диагностическое `:system worker pie ping` посылает HP1 обычный IRQ18 и позволяет
проверять сохранность результата под прерываниями. Это не команда запуска SIMD
в ISR. На одном serial port должен быть только один reader.

В default probe не запускается и scratch/report не выделяются. При исключении
на HP1 сначала сохранять state/phase/trap; восстановление выполняется reset,
а не повторным обращением к faulted worker.

## Дальнейшие проверки

1. QACC: независимые lane accumulators, насыщение/округление и выгрузка полного
   контекста. Затем fixed-point FIR/FFT/complex arithmetic.
2. Перестановки/packing, unaligned-load machinery, fused store, unsigned ALU,
   вложенные HWLOOP и разные границы I-cache. Отдельно проверять ограничения
   конфликта регистров fused-инструкций, не выполнять случайные кодировки.
3. Обычный SRAM-путь с D-cache ON, затем поддерживаемые пути PSRAM и размеры
   рабочих наборов. Текущие скорости получены на маленьком SRAM working set.
4. Сравнить скалярный SHA, возможную PIE-реализацию и отдельный SHA-периферийный
   ускоритель по одинаковым блокам, padding/endian и известным тест-векторам.

Для SHA-256 нужны mod2^32 addition, rotate, boolean operations и правильное
расписание. В нашем rev1.3 тесте `vadd.s32` **насыщает**, что не эквивалентно
SHA addition. Наличие четырёх32-битных lanes ещё не доказывает быстрый SHA.
Нельзя механически брать опциональный `sat` из ISA2.2 или переносить туда
новые инструкции. В IDF есть самостоятельный SHA HAL
`components/esp_hal_security/esp32p4/include/hal/sha_ll.h`; исследовать его
отдельно, включая стоимость MMIO, start/continue и сохранение digest/midstate.
Полный mining loop/nonces/hashrate пока не реализованы и не измерены.

## Измерения на плате

HP CPU160MHz, I0/I1 ON, D OFF, L2=0, normal SRAM scratch1008B.
В каждом batch128 вызовов; по4 измеряемых повтора после прогрева, чередуется
порядок scalar/PIE, сохраняется минимум целого batch. Ниже медианы10 прогонов
**с проверками SRAM-диапазона в публичном API**, под параллельным SD/HTTP чтением.
Обычные kernels скомпилированы текущим release `opt-level=z`; copy/xor используют
u32 доступы, int16 — saturating_add, dot — signed i64 sum. Это сравнение именно
с этими ядрами, а не с лучшей возможной scalar assembly или результатами IDF.

| Kernel | Scalar, циклов/batch | PIE, циклов/batch | Отношение |
|---|---:|---:|---:|
| Copy u32 ↔128-bit | 295977 | 100323 | 2.95× |
| Saturating int16 add | 1469358 | 167920 | 8.75× |
| XOR u32 ↔128-bit | 534673 | 169970 | 3.15× |
| Int16 dot →i64 | 1356650 | 122590 | 11.07× |
| Int16 dot fused →i64 | 1357252 | 100001 | 13.57× |

Внутренний рабочий набор маленький, инструкции прогреты, D-cache выключен;
эта таблица не описывает PSRAM, большие изображения или mining hashrate.
Fused вариант сокращает расходы на загрузку A, оставаясь корректным для
пустого и последнего блока. Незащищённые низкоуровневые вызовы в ранних пробах
были быстрее; для публичного API оставлены проверки, сравнивать числа отдельно.

Длительный тест:10×69510=695100 проверок, все18 счётчиков ошибок нулевые,
600 IRQ18 **во время** PIE, CSR2→3→0 в каждом прогоне. Ноут одновременно
выполнил182 полных SD/HTTP чтения HOME.JPG: SHA-256 каждый раз совпал,
`1165c20eef07d3d0c34140c686f919086381c6746f50d12a889910a3134b86c9`.
После этого121126 xorshift заданий проверены без ошибок. SPM pause/resume и
I1 cache disable/restore прошли; stack watermark560/8192, guard=true.
LP timer продолжает считать, background RTC/UI без ошибок.

115 host unit tests и target Clippy проходят. Артефакты и логи отдельных
сборок лежат в игнорируемом `target/pie-lab/`. В штатной прошивке probe выключен.

После проверок возвращён default image: web доступен, HP1 load=OFF/ошибок0,
LP period1000ms, каталог SD и SHA-256 HOME.JPG не изменились. LCD framebuffer
канарейки целы, PANEL_ERRORS count=0/overflow=false. Частота по повторному
CLOCK AUDIT159988101Hz, L1 bypass=0x10, L2 bypass=0x20, L2 size=0.
В default ELF отсутствуют pie kernels и диагностические scratch/report.
Финальная lab-сборка дополнительно проверила отказ запуска при PAUSED HP1,
resume и ещё69510 сравнений с нулевыми ошибками; default корректно отклоняет
`pie run` с сообщением о требуемом feature.

## Применение к сети

Отдельный copy-only владелец HP0 и feature `ethernet-pie` проверяют128-битные
копии CPU-owned Ethernet frames, сохраняя scalar fallback и DMA-контракт.
Высокоуровневый `Pie` для DSP остаётся закреплён за HP1. Результаты A/B/A
и требования ISR: [ethernet-pie.md](ethernet-pie.md).
