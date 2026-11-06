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

## Память и проверка

Новые крупные обработчики и старые диагностики помечены inline(never), чтобы
компилятор не складывал их временные буферы в постоянный stack frame rust_main.
До разделения его frame занимал около 8 KiB; после — 592 B.
BSS текущего образа — 0x5c80 B в 32 KiB DATA, остаётся 9088 B под стек.
Карта памяти не расширялась, ROM-области не затрагивались. Полного runtime
измерения максимальной глубины стека пока нет.

Последний прошитый образ: **114976 B**. 31 обычный host-тест прошёл,
проверка полного HOME.JPG через отдельный ignored test тоже прошла.
Файлы медиа, дампы и аппаратные логи остаются в ignored target/ и на карте.
