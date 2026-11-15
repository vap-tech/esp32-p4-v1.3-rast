# PPA BLEND: функциональная проверка, открытый RID

ESP32-P4 rev1.3, bare-metal Rust,2026-11-15. Feature `ppa-lab`, без C/IDF runtime
и новых зависимостей. Пиксельная часть заведена и проверена, но обычные кадры
пока не проходят строгий контроль AXI RID. В production/default не включено,
на LCD результат не публикуется. Это не законченный стабильный compositor.

## Реализация и контракт

`hal::ppa::blend_rgb565(background,foreground,output,Blend,descriptors,timer)`:
RGB565 background + straight ARGB8888 foreground → RGB565 output. Нативный
little-endian: ARGB8888 word `0xAARRGGBB` лежит в памяти как B,G,R,A. Background
непрозрачен. Входы, output и workspace — четыре различные CPU-owned области
SRAM/uncached PSRAM с базами64B-aligned. Проверяются длины, границы ROI и
пересечение DMA-адресов. In-place сейчас запрещён, даже если IDF разрешает.
Workspace≥192B: три дескриптора через64B. Фиксированные каналы TX0(BG),TX1(FG),
RX0(output), burst32B. Общий Claim сериализует весь2D-DMA с JPEG/SRM/FILL.

`Blend` задаёт размеры всех поверхностей, BG ROI, FG origin и output origin.
Размер блока общий для всех слоёв. BG/output: чётные pic_width, x и width ROI.
Для ARGB8888 FG допустимы нечётные ширина и x (естественное4B выравнивание).
Проверен FG35×25, origin(3,1), разные strides и смещения трёх областей.

Режимы `Alpha`:

- `Pixel`: исходная альфа каждого пикселя.
- `Fixed(u8)`: заменяет альфу всех пикселей, включая полностью прозрачные.
- `Scale(u16)`: умножение pixel_alpha*value/256.0..256;256 реализован через
  Pixel, а не через переполнение8-битного регистра.255 не эквивалентен256.
- `Invert`:255-original_alpha.

CPU не смешивает пиксели внутри HAL. PPA выполняет blend,2D-DMA читает/пишет.
Color-key окна явно отключены (low>high), чтобы настоящий чёрный участвовал
в смешивании. D-cache OFF,L2=0. Операция пока блокирующая, timeout1с. После
ошибки/EOF producer останавливается; оба TX и RX подтверждают reset-safe,
write FIFO опустошается перед успешным завершением. Если остановку подтвердить
невозможно — panic с удержанием буферов. Не освобождать память живого DMA.

## Строгий и диагностический вызов

Обычный `blend_rgb565` немедленно возвращает Hardware error при RID/RRESP/WRESP,
PPA parameter/size errors и DMA errors. На типичном кадре сейчас это ожидаемый
результат: PIXEL path сам по себе не делает операцию успешной.

Отдельный `probe_blend_rgb565` позволяет исследовать данные: только RID failure
откладывается до EOF; response/descriptor/FIFO/parameter/size errors остаются
немедленно фатальными. После quiesce RID всё равно превращает результат в Err.
Промежуточные ошибки накапливаются в `Status::axi_observed` (OR низких12бит,
не число ошибок), поэтому4-битный счётчик не может скрыться простым rollover.
`startup_axi` сохраняет три фазы запуска. Ошибки не очищаются перед отчётом.

Лаборатория продолжает сверку после Err только для завершённого RX EOF с
единственным типом ошибки RID. Итог явно сообщает
`Err("RID diagnostic: all pixel checks passed")`, а не Ok. Этот режим не следует
подключать к scanout/обычному рендеру, игнорируя ошибку.

## Что измерено

Все пиксели сравниваются с CPU alpha reference при opaque background:
`(Cb*(255-alpha)+Cf*alpha)/255`, затем упаковка в RGB565. Допускается1 native
шаг R/G/B только для промежуточной альфы;0/255 и область вне ROI совпадают точно.
Источник проверяется на неизменность, input/output/descriptors имеют canaries.

13режимов: Pixel;Fixed0/1/64/128/254/255;Scale0/1/128/255/256;Invert. Исходная
альфа содержит0/1/64/127/128/192/254/255, RGB меняется, включая настоящий чёрный.
Все13 режимов прошли, максимальная ошибка `[1,1,1]`, guard/source целы.
Стресс10×13=130 операций также прошёл пиксельные проверки, сохранив RID error.

| Диагностический кадр | Время аппаратной операции |
|---|---:|
| ROI20×12 внутри разных поверхностей |~70–99мкс |
|320×240, Fixed128 |9673мкс |
|1024×600, Pixel |79821мкс |
|1024×600, Scale128 |79260мкс |

Время включает setup/EOF/drain/abort, без выделения/инициализации и CPU-сверки.
LCD в это время читал прежний framebuffer. Большие выходы проверены полностью;
для Pixel полного кадра153600 пикселей имеют alpha0/255 и совпали точно.
Это диагностические замеры с RID error, не гарантия production throughput.
Минимальный2×1 также прошёл и дал чистый AXI status.

## Открытая проблема

У исходной конфигурации обычно startup `[0,0,0]`, после начала потока при двух
TX растёт RID_ERR_CNT. RX EOF приходит, оба TX завершаются, RRESP/WRESP=0,
PPA size/parameter errors=0; проверенные пиксели корректны. Малый ROI часто
заканчивается с RID=3, большой — с другим остатком4-битного счётчика.
Это не подтверждение безвредности счётчика и не установленная причина на RTL.

Не устранили RID в контрольных запусках:

- источники в SRAM вместо PSRAM, выровненный полный ROI вместо смещённого;
- TX0/1,TX0/2 и перестановка BG/FG между каналами;RX1 вместоRX0;
- burst8/16/32/128B, weighted arbitration, page-bound wrap off;
- явные непересекающиеся internal/external memory limits;
- descriptor burst off, отдельные channel soft resets;
- ожидание заполнения TX FIFO и read-command drain, завершения RX descriptor;
-50мкс между стадиями;10мс без чтения регистров DMA во время операции.
- явный CSC bypass и на TX1 по текущему IDF LL; результата не изменил,
  экспериментальная настройка в основной драйвер не перенесена.

Фиксированные задержки и альтернативные регистровые настройки в финальном
драйвере не оставлены. Причина пока не выяснена. Проверенный JPEG/SRM startup
workaround нельзя считать общим решением для двух одновременно читающих TX.
Контроль на штатном IDF BLEND также воспроизвёл RID, см. ниже.
SDK сам `axi_err` не проверяет.
Автоматически объявлять RID допустимым по одному совпадению пикселей нельзя.

## Контроль на ESP-IDF v6.1

На этой же rev1.3 запущено отдельное C-приложение с **неизменённым SDK**
`fff9895c82d744c7237be8847347bdd1b07c6643`. Использованы публичные
`ppa_register_client`/`ppa_do_blend`, blocking mode, burst32B. Это внешний
диагностический стенд, не зависимость Rust-прошивки. Исходный checkout IDF
пользователя не изменялся; копия SDK, toolchain и сборка находятся в игнорируемом
`target/idf-blend-reference/`.

Контрольная конфигурация: CPU360МГц, L2=128КиБ, все три поверхности во
внутренней SRAM, выравнивание64B и guard64B с обеих сторон. BG32×24 RGB565,
FG35×25 ARGB8888, output64×32; ROI20×12, начала BG(4,2),FG(3,1),output(8,5).
BG alpha Fixed255, FG Pixel. CPU-эталон и допуски те же, что в Rust-лаборатории.

| Проход | IDF result | RID до → после | Пиксели вне допуска | Max native error | Guard errors |
|---|---|---|---:|---:|---:|
|0|ESP_OK|0 → 2|0|1|0|
|1|ESP_OK|2 → 8|0|1|0|
|2|ESP_OK|8 → 12|0|1|0|

RRESP/WRESP во всех трёх случаях0. Это чистый SDK без промежуточных printf в
драйвере; лог `target/idf-blend-reference/usb-sram-driver.log`. Предыдущий
прогон с маркерами инициализации также дал правильные пиксели и RID2/8/14;
точные остатки счётчика не обязаны совпадать. Измеренные времена IDF здесь
не сравниваются со скоростью Rust: частота и конфигурация памяти различаются.

Повтор с PSRAM200МГц, штатным кешируемым отображением IDF, теми же ROI,
cache-msync и неизменённым SDK: все три операции ESP_OK, bad=0,max=1,guards=0;
RID13 после каждой (первый стартовал с0), RRESP/WRESP=0. Лог
`target/idf-blend-reference/usb-psram-driver.log`. Равные конечные значения
4-битного счётчика не доказывают отсутствие новых ошибок между чтениями.

Таким образом, ненулевой RID не специфичен для нашей bare-metal реализации,
PSRAM или LCD-нагрузки: он воспроизводится штатным драйвером на SRAM без LCD.
Это **не** доказательство безвредности ошибки и **не** подтверждённая errata RTL.
Строгий отказ Rust-драйвера сохранён. Следующий вопрос — семантика счётчика
при нескольких TX и подтверждение производителя, а не отключение проверки.

Для воспроизведения стенда нужен штатный USB Serial/JTAG driver с VFS;
используется `usb_serial_jtag_driver_install` и
`usb_serial_jtag_vfs_use_driver`, stdout unbuffered. Минимальный ROM-вывод
и `fflush` в первоначальных сборках зависали и мешали диагностике; эти
запуски не считаются результатами BLEND. SDK не патчится ради обхода RID.

Исходник отдельного стенда и инструкции сохранены в
[`scripts/labs/ppa-idf-reference`](../scripts/labs/ppa-idf-reference/README.md).
Cargo этот C-код не собирает и IDF не линкует.

## Команды

```
./scripts/image.sh --features ethernet-pie,jpeg-lab,ppa-lab
```

Остановить web через ESC, затем:

```
:display ppa blend                 # строгий вызов; ожидается Hardware error
:display ppa blend-probe           #13 режимов, pixel checks с явным RID error
:display ppa blend-large-probe     #2×1,320×240,два1024×600
:display ppa blend-stress-probe    #130 проверок с явным RID error
```

Ничего не пишет на SD и не переключает framebuffer. После тестов возвращается
стабильная Ethernet-PIE прошивка. База JPEG/PPA/BLEND сохранена в `117d428`.

Источники: локальные `esp_driver_ppa/src/ppa_blend.c`, `ppa_ll.h`, TRM rev1.3
раздел35.5.4; [официальная документация PPA](https://docs.espressif.com/projects/esp-idf/en/stable/esp32p4/api-reference/peripherals/ppa.html).
При исследовании просмотрены также отчёты об отдельных проблемах rev1.3:
[reset_avail отключённого канала](https://github.com/espressif/esp-idf/issues/19108)
и [SRM90/270 при x%4==2](https://github.com/espressif/esp-idf/issues/19096).
Они не доказывают причину нашего RID и требуют отдельного regression corpus.

Финальная регрессия: FILL→SRM→строгийBLEND(ожидаемый отказ)→FILL→JPEG test
(включая early-EOI timeout/recovery)→SRM-large→BLEND-large-probe(ожидаемый RID
при верных пикселях)→HOME.JPG. Все ожидаемые результаты подтверждены; JPEG
checksum не изменился. Стек53364/65536B, guard цел.122host tests и target Clippy
combined/normal/ppa-only прошли, fmt/diff-check чистые. Позднее выполнен
C-baseline со счётчиком, описанный выше; он также воспроизводит RID.
