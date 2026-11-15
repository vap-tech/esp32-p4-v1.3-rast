# C-reference: PPA и USB-дисплей

Статический разбор 2026-11-15: соседний checkout
`/home/kot/Projects/esp-32-p4-v1.3`, commit
`f95b8bdf54511c65ad7868dad1cebfb0574d00b5`,
origin `https://github.com/vap-tech/esp-32-p4-v1.3.git`, MIT.
Проект не изменяли, не собирали и не прошивали. Это reference на ESP-IDF 5.5.3,
не зависимость Rust-прошивки. Показатели ниже взяты из его документации,
не измерены заново. Наш локальный IDF v6.1 использован для сверки API PPA.

## Что полезно перенести

`main/ppa_compositor.c`: таблица поверхностей RGB565/ARGB8888, размеры, stride,
проверка прямоугольников, отдельные FILL/SRM/BLEND. PSRAM allocations выровнены
на64байта. PPA работает blocking через IDF; низкоуровневых регистров в обёртке
нет, их всё равно нужно поддержать нашим HAL по TRM/LL.

`main/protocol_v2.c`, `tools/gid2`, `host/guition`: GID2 с16-байтовым LE-заголовком,
sequence и length, GET_CAPS, создание/обновление поверхностей, fill/blit/scale/
blend/present, пакет FRAME до64операций и события тача. Эффекты и layout на ПК,
плата предоставляет примитивы. FRAME не транзакционен: ошибка останавливает
оставшиеся операции, уже выполненные не откатываются. Python host можно
использовать как тестовый клиент после согласования wire format.

`main/protocol.c`: USB OTG High-Speed vendor bulk, FS fallback, HS packet512B,
FS64B. Legacy interface0 endpoints02/82; GID2 interface1 endpoints04/84.
Это отдельный USB OTG-контроллер, не наш USB Serial/JTAG console. Режим не
объявляет стандартный монитор ОС: требуется host-приложение/протокол.

`main/usb_xfer_experiment.c`, `usb_rx_stream.c`: два RX-блока по32КиБ,
владелец OUT endpoint, completion→consumer→rearm, backpressure при отсутствии
свободного блока. IN-ответы хранятся в статической очереди32×128B до completion,
а не отправляются из временного stack buffer. Эти правила владения стоит
сохранить, заменив задачи/семафоры неблокирующими Rust-автоматами.

## Ограничения текущего reference

- Два DPI framebuffer действительно выделены. Но отдельный сериализованный
  front/back автомат и refresh callback, описанные в architecture.md, в текущем
  main не найдены. Surface0 привязана к fb0; PRESENT внешней поверхности вызывает
  draw_bitmap(), дальнейшее копирование/scanout принадлежит IDF. Не считать
  это доказанным zero-copy swap или запретом записи в активный framebuffer.
- s_lock в compositor создаётся, но не берётся. Нынешняя последовательная
  обработка команд не заменяет контракт владения для будущих нескольких клиентов.
- Операции blocking, burst128B. У нас были вспышки LCD при конкуренции за PSRAM:
  размер burst и одновременный scanout нужно проверять отдельно.
- BLEND принудительно задаёт alpha фона255 и foreground=параметр команды.
  Альфа отдельных пикселей ARGB8888 при этом заменяется; для прозрачных glyph/
  sprite нужен отдельный режим сохранения/масштабирования pixel alpha.
- BLIT запрещает src==dst, SCALE такого запрета не содержит. Для нашего HAL
  overlapping input/output запрещать до отдельного подтверждения аппаратуры.
- SCALE передаёт float dw/sw, dh/sh; IDF v6.1 квантует коэффициенты в fixed-point
  и рассчитывает фактический размер. Запрошенный dw/dh нельзя автоматически
  считать точным выходным размером, нужно возвращать/проверять quantized extent.
- RX slots используются несколькими задачами без общего lock всех переходов;
  reset берёт mutex, остальные обращения — нет. Для Rust явно синхронизировать
  reset/completion/consumer, а не копировать C state machine буквально.
- Протокол сначала собирает payload целиком в PSRAM, затем копирует строки
  в поверхность. Это не zero-copy USB→surface; максимум payload около2,46МБ.
  При ошибочном length нужен явный drain/reset framing; текущий handler
  возвращает ошибку, не прочитав payload.
- Большие тестовые/legacy buffers и задачи всё ещё запускаются вместе с USB;
  для production bare-metal оставить только используемый transport.

## Память и скорость

Один1024×600 RGB565 кадр =1228800B (~1,172МиБ), два ~2,344МиБ PSRAM.
ARGB8888 full-screen ~2,344МиБ. Базовое RX-кольцо2×32КиБ =64КиБ SRAM;
остальные USB statics и descriptors считаются дополнительно. Surface lifetime
должен включать все USB/PPA/LCD потребители. Uncached PSRAM убирает наши
cache clean/invalidate, но не требования к DMA ordering, lifetime и арбитражу.

README reference сообщает ~13FPS для полного raw кадра: это ~15,97МБ/с
полезных pixel bytes в полном тракте, а не измеренный предел USB.
Документированные FILL3,47мс, SCALE9,37мс, BLEND17,27мс, BLIT25,55мс — ориентиры
конкретного IDF-тракта. Там CPU360МГц и кешируемая PSRAM200МГц; наши160МГц /
uncached конфигурацию напрямую с ними не сравнивать.

## План нашего bring-up

1. AXI RID counter JPEG: startup workaround уже проверен ([jpeg.md](jpeg.md));
   учесть этот результат при нескольких TX-каналах PPA, не считать
   JPEG-последовательность готовым решением для произвольной PPA операции.
2. Offscreen PPA fill→copy→scale/rotate/mirror→blend, CPU эталоны, canaries,
   ошибки и recovery, затем одновременная работа с LCD. Разделить владение
   общей2D-DMA с JPEG, пока сериализовать операции.
3. Ввести подтверждённый frame-boundary handoff front/back.
4. Позже отдельный USB HS device HAL: enumeration/EP0→bulk RAM benchmark→
   двухблочный transport→GID2 surfaces/touch. Сначала чистая пропускная
   способность, затем pixel path и PPA, независимо от SD/HTTP.

USB-дисплей сейчас только оценён, код драйвера/режим в Rust не добавлены.
