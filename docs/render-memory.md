# Память, рендер и USB-видео: предложения для собственного HAL

Часть предложений уже реализована: один PSRAM framebuffer и прямой поток CSI/ISP —
[framebuffer.md](framebuffer.md). Остальное ниже — проект организации, не готовый API. Исходная точка этого плана — аппаратный VPG. Теперь PSRAM, framebuffer и DMA
подняты в минимальном режиме; статус проверенных функций указан в новом документе.
ESP-IDF/RTOS/LVGL в проект не добавляем.

## Разделение ответственности

Предлагаемые слои, без обязательного heap и сторонних зависимостей:

- `hal::psram`: инициализация памяти для rev 1.3, проверка размера и устойчивости.
- `hal::cache`: обслуживание диапазонов памяти перед/после DMA, размер и
  выравнивание cache line. Обычный Rust memory fence не заменяет writeback/invalidate.
- `hal::dma`: дескрипторы и передача памяти в DSI bridge; DMA2D для копирования
  изображений добавляется отдельно, когда появится измеренная необходимость.
- `graphics`: поверхность с width/height/stride/форматом и CPU-рисование
  прямоугольников, линий, текста. Векторные примитивы растеризуются в эту поверхность.
- `display`: владение framebuffer, начало/завершение кадра и безопасный `present`.
  Существующий драйвер JD9165 продолжает управлять панелью, RESET и подсветкой.
- Позже `jpeg` и `usb::host::uvc` либо `usb::device`: отдельные источники кадров,
  которые не навязывают графическому слою USB-протокол или JPEG.

Первые большие буферы можно выделить из фиксированной области PSRAM один раз
при старте. Нужны проверка границ/переполнения размера, выравнивание и описание
владения; полноценный allocator для framebuffer не обязателен. Дескрипторы и
USB transfer buffers размещаем только в памяти, доступной соответствующему DMA:
возможности разных DMA нельзя считать одинаковыми.

## Этапы запуска

1. Поднять PSRAM, провести read/write/address-pattern проверки до запуска DMA.
2. Один framebuffer RGB565 1024×600, непрерывное чтение через DMA → DSI bridge.
   Сначала неподвижная заливка, затем сетка, текст и счётчик кадров.
   Изменение единственного активного буфера может давать tearing; этот этап —
   проверка тракта памяти, а не окончательная модель анимации.
3. Два framebuffer: CPU рисует back, DMA читает front. `present` публикует
   полностью готовый кадр; переключение — на проверенной границе кадра.
   Старый front возвращается для записи только после фактического освобождения DMA.
4. Измерить время CPU-рисования, cache sync, пропускную способность и underrun.
   Затем решать, нужны ли dirty rectangles, DMA2D/PPA или третий framebuffer.
5. Подключать JPEG/USB только после независимой проверки стабильного scanout.

`draw complete`, `copy complete`, VSYNC и `buffer released` — разные события.
Не возвращаем приложению mutable-ссылку на буфер, который ещё читает DMA.
Полезная модель состояний: `Free → Drawing → Ready → Scanning → Free`.
Для двух буферов при pending present новый кадр либо ждёт освобождения,
либо возвращает Busy; активный буфер никогда не перезаписывается.
Три буфера дают front + готовый pending + рисуемый кадр, но требуют явной
политики замены pending. Для интерактивного UI разумно сохранять свежий кадр,
а не накапливать длинную FIFO, увеличивающую задержку.

Для собственной синхронизации используем событие DMA/bridge, когда проверим
его смысл на rev 1.3. TE панели пока не выведен на известный GPIO, DCS scanline
возвращает повреждённый ответ; строить `present` на них сейчас нельзя.

## Размеры и нагрузка для нашей панели

| Память | RGB565 | RGB888 |
|---|---:|---:|
| Один 1024×600 framebuffer | 1 228 800 B | 1 843 200 B |
| Два framebuffer | 2 457 600 B | 3 686 400 B |
| Три framebuffer | 3 686 400 B | 5 529 600 B |
| JPEG output capacity 1024×608 | 1 245 184 B | 1 867 776 B |

При 60 кадрах/с одно чтение видимых RGB565-пикселей требует 73,728 MB/s,
RGB888 — 110,592 MB/s (десятичные MB). Это нижняя оценка полезных данных,
без накладных расходов памяти. Полная перерисовка CPU добавляет запись;
отдельное полнокадровое копирование добавляет ещё чтение и запись.
Частота поступления новых кадров и постоянная частота развёртки LCD независимы.

Первый вариант — RGB565, два framebuffer после проверки одного.
Рисование сразу в back избавляет от отдельной промежуточной картинки.
Для частичных изменений нужно сохранять целостное состояние обоих буферов:
после смены front старый back может содержать устаревшие области. Можно
перерисовывать весь кадр, копировать актуальную базу или учитывать повреждённые
области для каждого буфера отдельно.

## Как устроен USB Extended Screen у Espressif

Проверен `esp-iot-solution`, commit
`5fbb2597b88f21ec41e952f81292ccd6ae569761` (срез исследования 2026-11-05).
Исходники сохранены в игнорируемом `old/reference/esp-usb-display/`.

Путь: PC render → JPEG → USB HS vendor → очередь JPEG → аппаратный JPEG decode
P4 → RGB decode buffer → `esp_lcd_panel_draw_bitmap` → LCD framebuffer → DMA/DSI.
В этом приложении нет локального LVGL/векторного рендера.

- `app_vendor.c`: пул из 6 JPEG-буферов, лимит по умолчанию 300 000 B на P4.
  Нет свободного буфера — кадр пропускается. Передача готовых кадров через очереди
  указателей, а не копирование их содержимого между задачами.
- `usb_frame.c`: сборка USB payload через `memcpy`; на P4 память выделяется
  `jpeg_alloc_decoder_mem`. В локальном IDF эта функция размещает её в PSRAM
  с выравниванием для кэша/DMA.
- `app_lcd.c`: отдельные RGB decode buffers. Kconfig default 2, но
  `sdkconfig.defaults.esp32p4` устанавливает 3. Размер резервируется с округлением
  обеих координат до 16; фактический layout определяется subsampling JPEG.
  При лишних пикселях справа строки уплотняются через `memmove`.
- LCD driver имеет собственные framebuffer; их число задаёт плата, не счётчик
  decode buffers. Поэтому «три буфера» здесь не означают всего три буфера в системе.
- Для 1024×600 RGB565 JPEG-пул + 3 decode buffers = 5 535 552 B, около 5,28 MiB,
  до framebuffer LCD, стеков и остальных выделений. Это расчёт по исходникам,
  а не измеренный heap устройства.
- В приложении нет ожидания VSYNC или callback завершения LCD-копирования.
  Кольцевая смена decode buffers сама по себе не доказывает отсутствие tearing
  и безопасное переиспользование при произвольном асинхронном backend.

В локальном IDF v6.1 DPI-драйвер выделяет framebuffer в PSRAM с DMA capability.
Для собственного framebuffer `draw_bitmap` может обойтись без копирования;
для внешнего — CPU-копирование либо зарегистрированный hook, включая DMA2D.
Cache sync выполняется в соответствующем драйвере. Наличие аппаратного DMA
на scanout не означает автоматическое применение DMA2D для каждого копирования.

Источники:

- [Extended Screen: app_vendor.c](https://github.com/espressif/esp-iot-solution/blob/5fbb2597b88f21ec41e952f81292ccd6ae569761/examples/usb/device/usb_extend_screen/main/app_vendor.c)
- [Extended Screen: usb_frame.c](https://github.com/espressif/esp-iot-solution/blob/5fbb2597b88f21ec41e952f81292ccd6ae569761/examples/usb/device/usb_extend_screen/main/usb_frame.c)
- [Extended Screen: app_lcd.c](https://github.com/espressif/esp-iot-solution/blob/5fbb2597b88f21ec41e952f81292ccd6ae569761/examples/usb/device/usb_extend_screen/main/app_lcd.c)
- [P4 defaults](https://github.com/espressif/esp-iot-solution/blob/5fbb2597b88f21ec41e952f81292ccd6ae569761/examples/usb/device/usb_extend_screen/sdkconfig.defaults.esp32p4)
- [IDF DPI driver](https://github.com/espressif/esp-idf/blob/fff9895c82d744c7237be8847347bdd1b07c6643/components/esp_lcd/dsi/esp_lcd_panel_dpi.c)
- [IDF JPEG decoder, allocation/cache sync](https://github.com/espressif/esp-idf/blob/fff9895c82d744c7237be8847347bdd1b07c6643/components/esp_driver_jpeg/jpeg_decode.c)

## USB-камера → P4 → LCD: сколько буферов

Рассмотрен именно host-пример `examples/usb/host/usb_camera_lcd_display`:
внешняя UVC-камера подключена к ESP как USB device, ESP работает хостом.
Это другая задача, чем сделать сам ESP веб-камерой для PC.

| Этап в актуальном host-примере | Количество | Назначение |
|---|---:|---|
| USB transfers (URB) | 3 × 10 KiB | Принятие USB-данных; это не полные RGB-кадры |
| Готовые/собираемые UVC MJPEG frames | 6 при PSRAM, 3 без неё | Пул сжатых кадров |
| Очередь приложения | 2 указателя | Ссылки на UVC frames, без ещё двух копий изображения |
| JPEG decode output на P4 | 1 | Отдельная выровненная RGB565-картинка |
| LCD framebuffer | 2 | Буферы, полученные из DPI-драйвера; приложение чередует их |

Итого на RGB-этапе действительно **три больших буфера: один decode + два LCD**,
но это не три равноправных framebuffer с тройной буферизацией.
Дополнительно существуют MJPEG-пул, URB и память драйверов.

`app_uvc.c` задаёт размер каждого MJPEG-буфера как
`max(width * height * 2 / 6, 40000)`. Для гипотетического поддерживаемого камерой
1024×600 это 204 800 B, 6 штук = 1 228 800 B. Это оценка JPEG-capacity,
не гарантия, что любой JPEG такого разрешения поместится: есть overflow event.
Ветка с PSRAM запрашивает `MALLOC_CAP_SPIRAM`; размещение URB определяет USB driver,
а не этот флаг для полных кадров.

Callback UVC кратко кладёт указатель в очередь. При полной очереди кадр
отбрасывается. Успешно переданный кадр остаётся во владении приложения до
`uvc_host_frame_return`, который вызывается после завершения frame callback.

На P4 `main.c` всегда выбирает отдельный decode output с округлением до 16.
Аппаратный JPEG → RGB565 → CPU `memcpy` построчно в back LCD buffer,
с центрированием → CPU-текст FPS через `esp_painter` → `draw_bitmap` → смена
индекса back. При передаче собственного LCD framebuffer дополнительной
полной копии внутри DPI `draw_bitmap` не требуется.
В этом примере отсутствует явное ожидание освобождения предыдущего front;
само чередование двух указателей не является доказательством безопасного present.

Входной формат примера — MJPEG. Выбираются разрешения не больше LCD;
на P4 нет масштабирования аппаратным JPEG-декодером в этом коде.
Для 1024×600 RGB565 суммы основных payload allocations:
6 JPEG + 1 decode + 2 LCD + 3 URB = 4 962 304 B, около 4,73 MiB.
Это арифметическая оценка для такого режима камеры, без allocator overhead,
дескрипторов и остальных структур; конкретная камера может его не поддерживать.

Также есть прежний `components/usb/usb_stream/examples/usb_stream_lcd_display`:
там видны `xfer_buffer_a`, `xfer_buffer_b` и `frame_buffer` по 88 KiB.
Это два USB payload buffers и один JPEG frame buffer, а не три RGB framebuffer.
Далее приложение использует отдельные JPEG ping-pong buffers и LCD buffers.
Нельзя переносить количество буферов из одного примера в другой без учёта ролей.

Источники того же зафиксированного commit:

- [Camera host: app_uvc.c — pool, URB, очередь и возврат кадров](https://github.com/espressif/esp-iot-solution/blob/5fbb2597b88f21ec41e952f81292ccd6ae569761/examples/usb/host/usb_camera_lcd_display/main/app_uvc.c)
- [Camera host: main.c — decode, копирование, overlay, два LCD buffers](https://github.com/espressif/esp-iot-solution/blob/5fbb2597b88f21ec41e952f81292ccd6ae569761/examples/usb/host/usb_camera_lcd_display/main/main.c)
- [Camera host: sdkconfig.defaults — PSRAM и два LCD buffers](https://github.com/espressif/esp-iot-solution/blob/5fbb2597b88f21ec41e952f81292ccd6ae569761/examples/usb/host/usb_camera_lcd_display/sdkconfig.defaults)
- [Прежний usb_stream_lcd_display](https://github.com/espressif/esp-iot-solution/blob/5fbb2597b88f21ec41e952f81292ccd6ae569761/components/usb/usb_stream/examples/usb_stream_lcd_display/main/main.c)

Для нашего HAL сначала достаточно собственных CPU-рисования и scanout.
Если позднее потребуется UVC, отдельно выбираем глубину MJPEG-пула по измерениям
потерь и задержки. Начальное предложение: короткая очередь готовых кадров,
явное владение каждым буфером, сброс устаревших кадров для live preview.
Прямой JPEG decode в back потенциально убирает промежуточный RGB-буфер/копию,
но требует совместимости stride/MCU padding, выравнивания, cache ownership
и надёжного освобождения framebuffer. Это оптимизация для отдельной проверки.
