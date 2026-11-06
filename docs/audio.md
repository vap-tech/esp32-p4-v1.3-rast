# Аудио: I²S1 TX → ES8311 → NS4150

2026-11-06. Собственный Rust HAL без зависимостей, ESP-IDF/ADF runtime,
RTOS и прерываний. Первый этап — короткий тестовый тон. Цифровая передача
на плате проверена. **Пользователь подтвердил приятный короткий тон из динамика**
после исправления PA_CTRL на GPIO11.

## Подключение

Аудиораспиновка нашего экземпляра совпала со схемой I_W_Y; старая I_W отличается:

| Сигнал | GPIO/адрес |
| --- | --- |
| ES8311 I²C | I2C1 SDA7/SCL8, 7-bit 0x18 |
| I²S1 MCLK | GPIO13 |
| I²S1 BCLK | GPIO12 |
| I²S1 WS/LRCLK | GPIO10 |
| I²S1 DOUT → кодек | GPIO9 |
| Кодек → I²S DIN | GPIO48, проверен цифровой DAC loopback |
| NS4150 PA_CTRL | **GPIO11**, high=включён, low=standby |

NS4150 — отдельный усилитель между дифференциальным DAC ES8311 и динамиком,
питается от VOUT-BAT по схемам. P4 выступает I²S master, кодек slave.
GPIO20 для усилителя в текущем коде не используется: это неверная для нашего
экземпляра привязка из I_W. USB Serial/JTAG остаётся на GPIO24/25.

## Тактирование и формат

На P4 rev1.3 локальный ESP-IDF явно выбирает XTAL40 для I²S: PLL-источники
по умолчанию предназначены для новых ревизий. Мы PLL для аудио не включаем.

- XTAL 40 MHz / (9 + 49/64) = **4.096 MHz MCLK** в среднем.
- Аппаратные дробные поля: N=9, X=3, Y=4, Z=15, YN1=1.
- Сохранён SDK workaround двойного деления: сначала малый делитель, затем
  целевые поля в установленном порядке. Поля RX/других периферий сохраняются.
- BCLK = MCLK/8 = **512 kHz**.
- Два слота по 16 бит → **16 kHz LRCLK**.
- Signed PCM16 LE, левый/правый отсчёты упакованы в один u32; оба слота
  Philips I²S с задержкой MSB на один BCLK.
- ES8311: MCLK=256fs, DAC OSR=0x20, 16-bit I²S, вход GPIO9, ADC serial output
  выключен вне диагностического чтения. Перед open кодека I²S уже запущен,
  как в рабочем архивном захвате; захвата микрофона пока нет.

Частоты здесь расчётные по делителям. Осциллографом MCLK/BCLK/LRCLK не измерены.
Дробный делитель даёт усреднённую частоту; аудио-качество и джиттер ещё не оценены.

## DMA и владение

`hal::i2s::Tx` использует **AHB GDMA TX channel 0**, base 0x50085000,
trigger I2S1=4. Это другой контроллер, чем DW-GDMA камеры/дисплея и IDMAC SDMMC.
I²S1 base=0x500c7000. Глобального reset всего DMA нет — настраивается только TX0.

`unsafe Tx::new(frames, timer)` копирует от 1 до 512 упакованных PCM16 stereo
кадров в собственный статический SRAM-буфер, выровненный на 64 B. Аппаратно
буфер повторяется до stop(). Один 64-byte-aligned дескриптор ссылается сам
на себя, size/length равны длине данных, EOF/OWN установлены. Включена проверка
OWN, auto writeback выключен — неизменяемый кольцевой буфер не требует ISR
для повторного владения. Перед стартом выполнен L1 SRAM cache sync.

Память не освобождается под DMA. start ждёт bounded I²S UPDATE, stop — DMA PARK;
при таймауте stop делается сброс только этого TX-канала. После остановки
выключается функциональный TX/MCLK clock, bus clock остаётся для диагностики.
Все программные ожидания ограничены 100 ms. Конструктор требует исключительного
владения I²S1, парой AHB DMA0 (TX/RX), статической памятью,
GPIO9/10/12/13/48 и cache-sync регистрами.

Это пока циклическое воспроизведение короткого буфера. Потокового PCM API,
пинг-понг буферов и воспроизведения WAV с SD пока нет.

## Проба динамика

Динамик физически подключил пользователь. Команда **p** запускает пробу вручную:

```sh
python3 scripts/display-control.py p
```

На загрузке звук не включается. Проба:

1. PA_CTRL(GPIO11)=low; включить DMA/I²S до настройки кодека, проверить
   ID ES8311 (`83 11`, версия 01), записать настройки DAC и оставить soft mute.
2. Выдержать 50 ms.
3. Включить усилитель, снять mute, воспроизводить 2 s.
4. Вернуть mute, выдержать 20 ms, выключить PA, остановить I²S/DMA и MCLK.
   PA выключается и при ошибках.

Приложение генерирует синус 1 kHz (16 отсчётов на период), одинаковый в обоих
каналах. Буфер 512 кадров/2048 B — 32 целых периода. Пик 4096/32768 ≈ -18 dBFS,
DAC register32=0x8f = **-24 dB**. Это цифровые уровни, не измеренная мощность
динамика и не процент воспринимаемой громкости.

### Поиск причины тишины и подтверждённый результат

Первоначально использованы PA20 и 32-bit slots. DMA/I²S работали, но пользователь
сообщил полную тишину. Сравнение с `old/esp32-p4-voice-device/main/audio_voice.c`:
16 kHz, 16-bit slots, mono, MCLK256fs и I²S включается до открытия кодека.
Старый код использовал только захват; PA был отключён (`pa_pin=-1`), поэтому
сам архив не подтверждал правильность GPIO20 для динамика.

Привели wire-format к 16-bit slots и запуску тактов до codec init.
GPIO pad readback подтвердил активность DOUT/WS/BCLK/MCLK и PA high.
Само по себе это ещё не подтверждало получение PCM кодеком.

Добавлена короткая RX-проба через AHB DMA RX0, с общими TX BCLK/WS и GPIO matrix:

- GPIO9 → RX: собственный I²S PCM принимается обратно.
- ES8311 register44=0x68 (DACL+DACR), register0A=0x0c (SDOUT unmuted):
  GPIO48 → RX возвращает правильную синусоидальную последовательность
  `0000,061f,0b50,0ec8,1000,0ec8,...` в обоих каналах.
- RX буфер 256 B / 64 stereo frames, descriptor=0x40100100: OWN снят,
  length=256, EOF установлен; DMA raw=3. После EOF RX останавливается,
  память инвалидируется перед чтением CPU.
- Первый принятый кадр может быть неполным из-за старта посреди LRCLK,
  использовать его как аудиосэмпл нельзя. Полный выровненный capture-stream
  пока не реализован. Для цифровой проверки анализируются последующие кадры.
- После пробы register44 возвращается в 0x58 (как старое no_dac_ref=false),
  SDOUT снова muted. 0x44 управляет источником/референсом цифрового ADC output,
  **это не бит включения аналогового усилителя**.

В первичной схеме I_W_Y обнаружены **PA_CTRL=GPIO11, ES8311 SDOUT=GPIO48**.
Это совпало с работающим RX48. Замена PA20 на PA11 дала слышимый тон:
пользователь подтвердил приятный сигнал длительностью около 1–2 s.
Полное совпадение прочих частей PCB со схемой I_W_Y этим не доказано.

Последняя проба (`target/audio-pa11.log`):

- DAC=-24 dB, peak PCM=-18 dBFS, 1 kHz, 16 kHz stereo/16-bit slots.
- Snapshot 00/01/02/04/06/08/09/12/31/32/44/FF до unmute:
  `80 3f 00 20 03 ff 0c 00 60 8f 58 01`.
- FIFO delta=64646, BCLK delta=1034345 (окно чуть больше 2 s из-за диагностики).
- AHB TX/RX raw=3, TX descriptor error=0, I²S raw=0.
- В конце PA=false и DMA TX link=8 (PARK).

До изменения PA совместные проверки с LCD/FAT32 проходили:
framebuffer guards=true, camera=false, `LOGOS/DUMMY.565` hash=2621ce57.
Файлы SD при аудиопробах не записывались.

Логи: `target/audio-tone-first.log`, `target/audio-tone-final.log` (первоначальная
тишина), `target/audio-pins.log`, `target/audio-old-format.log`,
`target/audio-rx-first.log`, `target/audio-pa11.log`.
Финальный образ повторно прошит и проверен с LCD framebuffer:
`target/audio-confirmed.log`, обе RX-пробы успешны, p=Ok, framebuffer guards=true,
camera=false. Проверяются снятие RX OWN и полная длина дескриптора 256 B;
после cache invalidate добавлены fences. В конце PA11 выключен.
26 host-тестов, fmt и clippy -D warnings проходят.

## Ещё не реализовано

- Проверка частот/аналогового выхода осциллографом и качества под нагрузкой.
- Доведение потокового SD → PCM → I²S: WAV parser добавлен, кольцо DMA пока
  останавливается с descriptor error; слышимый WAV не подтверждён. См. [media.md](media.md).
- Регулировка громкости API.
- Непрерывный I²S RX, запись аналогового микрофона, duplex/echo cancellation.
- Другие sample rates/форматы, согласование смены тактов с кодеком.
- Контроль пропусков при длительной нагрузке и управление аналоговым питанием
  кодека. После пробы DAC muted и PA off, полного power-down кодека пока нет.

## Источники

Локальный ESP-IDF v6.1 `fff9895c82d744c7237be8847347bdd1b07c6643`:
`esp_hal_i2s/esp32p4/include/hal/i2s_ll.h`, `esp_hal_i2s/i2s_hal.c`,
`esp_hal_dma/esp32p4/include/hal/{gdma_ll,ahb_dma_ll,gdma_channel}.h`,
`hw_ver1/soc/{i2s,ahb_dma,hp_sys_clkrst,lp_clkrst}_reg.h`.

Последовательность кодека и шкала DAC volume сверены с
[Espressif esp_codec_dev ES8311](https://github.com/espressif/esp-adf/blob/68ce88ad4dcf2cadc3f2d45635de5d4305e51498/components/esp_codec_dev/device/es8311/es8311.c),
Apache-2.0, commit `68ce88ad4dcf2cadc3f2d45635de5d4305e51498`.
Локальная копия `/tmp/esp32-p4-audio-reference/es8311-pinned.c`,
SHA256 `0da6374b8b3bb1dea81b2d2077c2f95683bfb39077086e162f5a1bc98749ca67`.
Ни SDK runtime, ни C-код не включены в сборку.

Схема с правильным PA11:
[I_W_Y-V1.0](https://github.com/wegi1/ESP32P4-JC1060P470C-I_W_Y/blob/main/5-Schematic/JC1060P470C_I_W_Y-V1.0.pdf).
Регистры цифрового loopback сверены с
[ES8311 datasheet](https://files.waveshare.com/wiki/common/ES8311.DS.pdf)
и [User Guide](https://files.waveshare.com/wiki/common/ES8311.user.Guide.pdf).

## DMA streaming update

The earlier two-block SD playback descriptor error is resolved with a four-block
ring, owner check enabled, double cache maintenance and explicit OWN handoff.
60 s SD WAV + LCD and generated PCM + JPEG/LCD mixed tests passed. Underrun
stops DMA; a subsequent playback command reinitializes the channel. No automatic
resume of the same file is implemented. [Current validation and commands](dma.md).

## Live WAV and digital comparison

Current default DAC reg32=**0x95 / -21 dB**, raised 3 dB at user request.
The 0x8F/-24 dB figures above describe the earlier quiet baseline. Real WAV with
SD JPEG/LCD now passed continuous GPIO9 capture in both cache configurations.
[Sample continuity, ring metrics, negative tests and limits](wav-lab.md).
