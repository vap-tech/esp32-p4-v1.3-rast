# ESP32-C6: ESP-Hosted без SDK на P4

> Сейчас Wi-Fi не входит в default. Вернуть: `./scripts/image.sh --features wifi`.
> Для raw mode добавить diagnostics. [Полная инструкция и сохранённые зависимости](build-flavors.md).

2026-11-13. Работают SDIO transport, startup, RPC v2, scan и STA + DHCP/ARP/ping.
P4 остаётся bare-metal Rust, без heap, RTOS, Cargo-зависимостей или линковки IDF.
Используется установленная пользователем сопроцессорная прошивка C6.

## Проверенная разводка и запуск

| Сигнал P4 | GPIO |
| --- | ---: |
| SDMMC slot1 CLK | 18 |
| CMD | 19 |
| D0/D1/D2/D3 | 14/15/16/17 |
| C6 reset | 54, **активный низкий**, проверено на этой плате |

GPIO matrix input base именно `0x500e0158`, выход base `0x500e0558`.
Для slot1 источник в CLKSRC.card1=1 выбирает divider1; одного CLKDIV1 недостаточно.
Константы matrix ZERO/ONE на P4:0x3e/0x3f (не0x38).
Эти исправления внесены и в общий host; работа SD slot0 после них проверена.

Ранние попытки с неверной маршрутизацией CMD и противоположной полярностью
reset получали CMD5 response timeout. Описания других плат с тем же названием
не заменяют измерение на нашем экземпляре. С активным LOW получены CMD5,
CCCR и настоящий стартовый пакет C6; это подтверждает рабочую полярность.

Инициализация: reset20 мс → пауза1 с → host400 кГц/1 бит → CMD0/CMD5/R4,
CMD3/R6, CMD7 → CMD52 CCCR → enable function1, block size512, interrupts enable
в C6 (CPU IRQ не включается) → CCCR4bit → host4bit/25 МГц → OPEN_DATA_PATH.
Источник host — SDIO PLL200 /2, затем делители SDMMC. Высокоскоростной режим
C6/50 МГц пока не включён. Команды и DMA имеют таймауты.

```sh
python3 scripts/display-control.py --wifi-probe --hold 0
python3 scripts/display-control.py --wifi-scan --hold 0
```

Консоль: `:wifi probe` и `:wifi scan`. Диагностическая `wifi probe high`
оставлена для явной проверки другой полярности на других модулях; для нашей
платы она нерабочая, нормальная команда/скрипт по умолчанию выбирает low.
Не запускается автоматически при загрузке. Камера/активный будильник и занятый
DMA pool блокируют команду. D-cache требуется OFF; I-cache допустим.

## Что ответил наш C6

* OCR `0xa0ffff00`, CCCR `[32,02,00,02,...]`, chip ID0x0d (ESP32-C6).
* ESP-Hosted **3.0.9**, RPC version **2**, wire headerV1 длиной12 байт.
* capability0x01, ext0x10: Wi-Fi SDIO; Bluetooth не объявлен.
* SDIO mode1, буферный TLV `[0,0,31,0,31]`: C6 сообщает агрегаты/буферы31×512B.
* AppGetDesc RPC: проект `eh_cp_ota_coprocessor_ota`, IDF`v6.1`,
  версия приложения `31ddd83-dirty`. Это отдельно от версии ESP-Hosted3.0.9.

RX: читаем cumulative packet-length0x60 и вычитаем собственный счётчик modulo
2^20. FIFO CMD53 address=`0x1f800-actual_length`, DMA округляется до512B.
TX: rolling credits из0x44[27:16], собственный счётчик modulo4096;
без свободного буфера передача не запускается. На P4 один статический RX16 КиБ,
общий с SD bounce8 КиБ, статический TX2 КиБ и stack reply1024B. Неограниченных очередей нет.
Отправка host capabilities подтверждает chip/RPC, отключает raw-throughput
тест и задаёт пороги throttling0. Синхронный один RPC за раз с UID и проверкой
response ID; посторонние асинхронные события пропускаются с диагностикой.

`src/net/hosted.rs`: проверка bounds, optional byte-sum checksum, boot TLVs,
serial envelope и минимальный bounded protobuf wire codec. Отдельный сгенерированный
protobuf runtime не нужен. Fragments, неподдерживаемый peer/RPC, слишком большие
агрегаты/ответы дают явную ошибку вместо выхода за границы.

## Сканирование

AppGetDesc267 → WifiInit278 → SetMode260(STA) → Start280 →
ScanStart286(default config, blocking) → GetApRecords289(до8 записей) →
Stop281 → Deinit279. В Wi-Fi init явно nvs_enable=0, сетевые настройки не
сохраняются. Наш код не обновляет/стирает firmware C6. Собственная прошивка C6
может выполнять внутреннюю PHY-калибровку по своим правилам; полного запрета
любых её внутренних NVS-записей эта настройка не обещает.

На первом реальном прогоне найдены3 точки доступа, каналы1/6/9,
RSSI−74/−74/−78 dBm. Все RPC, включая stop/deinit, вернули успех.
Пиковый стек самого scan4848/16384B, guard intact. После возврата SD slot0
прошёл повторную инициализацию и чтение первого МиБ MKV блоками8192 и777B:
FNV`da757a6c` совпал с исходным, размер2490131073B сохранился.
SD-карта в этих опытах используется только для чтения.

## Что ещё не сделано

* Hosted fragmentation/reassembly: ответ RPC ограничен1 КиБ; превышение
  отвергается. RX16 КиБ принимает объявленные15,5 КиБ порциями IDMAC до8 КиБ,
  но максимальный агрегат под нагрузкой отдельно ещё не проверен.
* D-cache ON, CPU IRQ/SDIO DAT1 interrupt вместо polling, power-save/reconnect.
* Bluetooth: текущая C6-прошивка его не объявляет.

TCP/HTTP и общий SDMMC host теперь обслуживаются кооперативно; см. запуск ниже.
DHCP/ARP/ICMP/TCP используют MAC выбранного интерфейса.

## Источники

* [Espressif ESP-Hosted MCU](https://github.com/espressif/esp-hosted-mcu),
  локально `reference/esp-hosted-mcu`, snapshot
  `4e55cc677f1ef14080b717abcc115c2aaf6a52c5`, каталог игнорируется Git.
* `common/eh_common/`, `common/eh_tlv/`, `common/serializers/eh_proto_v2/`:
  wire header, TLV tags, RPC schema. Справочник текущего upstream новее C6;
  поддерживаемые поля сверены с реально принятым стартовым пакетом/RPC.
* `port/os/idf/src/eh_host_port_sdio.c` и
  `host/mcu/eh_host_mcu_transport/src/eh_host_bus_sdio.c`: counters/FIFO/credits.
* Локальный IDFv6.1 `esp_hal_sd/esp32p4/`, `soc/esp32p4/.../gpio_reg.h`:
  матрица, clock selectors, SDMMC register semantics.
* Старый `old/esp32-p4-voice-device/main/wifi_scan.c` использовал ESP-Hosted
  init/connect и Wi-Fi scan/connect. Его sdkconfig с конкретной разводкой не
  сохранился, но пользователь подтвердил работу Wi-Fi на прежней прошивке.

Финальные повторные проверки: probe, два scan (I-cache OFF и ON), затем
повторное чтение SD. Все завершились успешно. В последней сборке watermark
scan4912/16384B, guards целы. Для исходной политики I-cache возвращён OFF.
Логи сохранены локально в игнорируемом `target/c6/`. Host tests —81 passed,
2 ignored; Clippy all-features без замечаний. Команда скрипта при Ctrl-C
посылает ESC и ждёт завершения/очистки, не дёргает аппаратный reset.


## STA и IPv4, 2026-11-13

```sh
python3 scripts/display-control.py --wifi-connect YOUR_SSID --wifi-seconds 300
# Пароль вводится через getpass без echo; в аргументах процесса пароля нет.
# Для автоматизации: --wifi-password-stdin, пароль из stdin, не из argv.
```

В USB передаётся ASCII hex SSID/password и длительность. Это кодирование,
не шифрование; логгер не печатает ни строку команды, ни credentials. Длина
консольного кадра увеличена96→256B для32-байтного SSID и64-байтного PSK.
После возврата credential-команды её входной буфер очищается; дополнительные
копии во время сериализации/передачи находятся только в оперативной памяти.
При использовании `--wifi-connect` введённый пароль не записывается в
source/config/docs/SD/Flash P4. Для явно разрешённого владельцем встроенного
профиля действует отдельный режим, описанный ниже. nvs_enable=0 для
Wi-Fi C6, прошивка/OTA C6 не меняются. Автоподключения после reset пока нет.
Поддерживаются WPA2+ и пароль8..63 байта либо64 hex PSK; открытые сети этим
интерфейсом намеренно не описаны. SSID до32 байт, кодированиеUTF-8 у helper.

Последовательность: boot/host capabilities → Wi-Fi init(modeSTA) → Start →
SetPs270(WIFI_PS_NONE для предсказуемого bring-up) → GetMac257 → SetConfig284
(SSID/password в RAM, WPA2 threshold, optional PMF) → Connect282.
Асинхронные события775/776 отмечают connect/disconnect. В RPC-приёме весь
агрегат обрабатывается до возврата ответа, чтобы не потерять connect event,
который оказался после ACK в том же CMD53.

После association свой DHCP Discover/Request и проверка ACK по xid/MAC,
собственный ARP responder и ICMP Echo. Функции `net::*_for_mac` принимают MAC
C6; Ethernet wrappers продолжают использовать прежний MAC. Ответы на ping
не обрабатываются lwIP/IDF на P4. Команда --wifi-default обслуживает DHCP/ARP/ICMP; HTTP включается отдельной --wifi-web. DNS-клиента нет.
Окно1..3600с начинается после DHCP, ограничивается lease. Association/DHCP
ограничены45с; автоматического reconnect/renewal пока нет. ESC завершает окно.
По окончании/ошибке выполняются Disconnect283, Stop281, Deinit279.
Другие команды/SD/обновление UI пока не обслуживаются внутри этого окна;
LCD scanout продолжается аппаратно.

### Проверка реальной сети

Соединение с заданной пользователем WPA2 AP, IP `192.168.51.31/24`,
router`192.168.51.1`, заводской C6 MAC`14:c1:9f:e6:5a:bc`.
Первое90-секундное окно с дефолтным modem power save:30/30 обычных ping,
12/12 с payload1400B (DF), без потерь. RTT обычных10,9/107,4/228,2мс.
После окна все cleanup RPC успешны, stack5808/16384, guard intact.
Повтор с modem power-save OFF:50/50 обычных и10/10 больших без потерь,
обычные RTT2,84/67,33/191,60мс, большие10,28/33,24/125,91мс.
Это путь через Wi-Fi ноутбука и маршрутизатор между подсетями, а не изолированное
измерение задержки P4/C6. Полного объяснения jitter пока нет.
Логи ping — ignored `target/c6/`; credentials туда не записывались.

Окно с PS OFF также завершилось штатно: rx63, replies61,
Disconnect/Stop/Deinit успешны, stack6192/16384B, guard intact.
До того же роутера192.168.51.1 с ноутбука20/20 ping, RTT0,61/1,00/2,00мс:
большой jitter до борды не объясняется одним только общим путём до роутера.
После теста радио отключено; автоматического подключения/постоянного профиля
не добавляли. Проверки после этого изменения:83 Rust tests passed,2 ignored,
Clippy all-features чистый; frontend/packer tests также прошли.


## Встроенный профиль, явно разрешён владельцем

Текущий SSID `R4-2G-TEST` и разрешённый владельцем пароль сохранены в `src/wifi_profile.rs` и входят в образ
Flash P4. При сборке профиль постоянный; в логах пароль не печатается.
Сопроцессор по-прежнему получает конфигурацию в RAM, nvs_enable=0.

```sh
python3 scripts/display-control.py --wifi-default --wifi-seconds 300
```

Консоль: `:wifi default 300`. Ввод пароля не нужен. Обычный `--wifi-connect`
с запросом пароля остаётся доступен для другой сети. Автозапуск при boot не
включён; окно и очистка disconnect/stop/deinit остаются прежними.


## Веб-файлы через Wi-Fi

```sh
python3 scripts/display-control.py --wifi-web 3600
```

Консоль: `:net wifi serve 3600`. Используется встроенный профиль, адрес выдаёт
DHCP (в проверенной сети http://192.168.51.31/). Сервер общий с Ethernet:
каталоги, preview/download, потоковая загрузка, mkdir, rename, delete,
буфер обмена и яркость. FAT32 и ASCII 8.3 остаются прежними ограничениями.

SD slot0 и C6 slot1 используют один SDMMC host. Инициализируется сначала SD,
затем радио; каждый слот восстанавливает свой clock divider и ширину.
Команды и общий IDMAC workspace строго сериализованы одним владельцем Timer.
Контроллер между пакетами не сбрасывается. Буфер принятого агрегата C6 хранится
отдельно от SD DMA workspace. Декодер принимает плотные и выровненные на 4 байта
кадры; повреждённый остаток отбрасывается, а не повторяется бесконечно.

D-cache OFF, L2=0; power save C6 отключён. Сейчас это polling и один обработчик
HTTP, без фоновых часов/аудио, reconnect и DHCP renewal. Автозапуска при reset нет.
Окно 1..3600 секунд, ESC/Ctrl-C завершает HTTP и disconnect/stop/deinit C6.
Доступ без авторизации внутри доверенной LAN. Скорость ещё не оптимизирована.

TCP/IP файлового сервера теперь smoltcp0.14.0 без std/alloc/async; окна и проверки
описаны в [web.md](web.md). DHCP bring-up и ESP-Hosted остаются собственными.

## SDIO profiling и чистый транспорт

В конце каждого RAM HTTP-теста USB выдаёт `C6 NET us=... Stats {...}`:

- `reg_reads/reg_us`: число32-битных чтений и время целиком внутри HAL.
- `rx_polls/rx_empty`: опросы длины и пустые ответы.
- `rx_transfers/rx_bytes/rx_us`: принятые агрегаты, Hosted-байты и время;
  один агрегат более8КиБ использует несколько DMA-команд.
- `tx_transfers/tx_bytes/tx_us`: переданные Hosted-кадры и время HAL.
- `credit_stalls`: опросы без свободного кредита; `credit_wait_us` — интервалы
  ожидания с регистрами и sleep (пересекается с `reg_us`, складывать нельзя).

Счётчики снимаются перед обработчиком и после finish; UART-логов на каждый
пакет нет. Аппаратное время, DMA setup, копирование и ожидание включены в HAL
время; это не измерение занятости проводов логическим анализатором.
Большая доля пустого polling сама по себе не доказывает, что SDIO узкое место:
CPU может так ждать пакеты, задержанные TCP/радио/AP.

Чтение регистра по умолчанию теперь CMD53 byte-mode на4B (одна транзакция).
Контрольная сборка с прежними четырьмя CMD52:

```sh
./scripts/image.sh --features sdio-reg-cmd52
```

Обычный `./scripts/image.sh` собирает CMD53. Исправлен BLKSIZ: в byte-mode
равен длине операции, в block-mode остаётся512. Общий DMA workspace и
сериализация с SD slot0 сохранены.

На той же сети R4-2G-TEST,25МГц/4бит,1МиБ в каждом направлении:

| Порядок | Регистр | Download КиБ/с | Upload КиБ/с |
|---|---|---:|---:|
| A1 | CMD52×4 | 218,82 | 126,29 |
| B1 | CMD53×1 | 57,16 | 79,26 |
| B2 | CMD53×1 | 64,21 | 59,91 |
| A2 | CMD52×4 | 43,92 | 50,43 |

Среднее время чтения регистра45–46мкс (CMD52) против43мкс (CMD53).
Большого выигрыша CMD53 не дал; разброс HTTP существенно больше разницы
команд, а контрольный CMD52 тоже замедлился. Не приписывать разброс смене API.
Почти все RX polls пустые; в HTTP прогоне около55–62% elapsed приходится на
чтение регистров. Ожидание кредитов при POST отсутствовало, при GET небольшое.

### Raw P4↔C6 без Wi-Fi/TCP/SD

Завершить web-run через ESC/Ctrl-C и дождаться cleanup. Затем:

```sh
python3 scripts/display-control.py --wifi-raw rx --raw-seconds 10
python3 scripts/display-control.py --wifi-raw tx --raw-seconds 10
python3 scripts/display-control.py --wifi-web 3600
```

Консоль: `:wifi raw rx 10` / `:wifi raw tx 10`. Направление относительно P4.
Время1..30с. RX ждёт первый кадр до8с, не включает задержку запуска producer
в измеренный интервал. C6 boot TLV0x13 объявляет raw_tp=1; host caps TLV0x46
включает RX(2)/TX(4), интерфейс TEST=6. Не выполняются wifi_init/connect,
не читается карта. Кадры разбираются с обычным Hosted framing.
По завершении/ESC/ошибке отправляется stop(0), затем аппаратный reset C6 и
переинициализация SDIO, чтобы producer/FIFO не остались активными. C6 Flash
не меняется. Проверяется объявленная capability, без неё запуск отвергается.

Фактический результат CMD53-регистров на25МГц:

| Направление | Payload | Время | Скорость |
|---|---:|---:|---:|
| C6 → P4 | 60736000B,41600кадров | 10,001492с | 6,07МБ/с /48,58Мбит/с |
| P4 → C6 | 33404800B,22880кадров | 10,000067с | 3,34МБ/с /26,72Мбит/с |

Это десятичные МБ/с и payload тестовых кадров без Hosted header. TX означает
успешную публикацию в SDIO FIFO с соблюдением возвращаемых кредитов; отдельных
receiver counters C6 мы через его UART не считывали. Сквозная проверка потерь
по payload sequence пока не реализована; отсутствие HAL/framing errors не
равно доказательству нулевых потерь. После обоих тестов cleanup успешен,
stack13844/16384B,guard intact; Wi-Fi/HTTP после raw поднялись повторно.

Вывод: на нашей плате программный SDIO-тракт способен на десятки Мбит/с уже
при25МГц. Это исключает низкий предел пропускной способности шины как объяснение
десятков КиБ/с HTTP, но не исключает задержки/взаимодействия драйвера под сетевой
нагрузкой. Следующие разделяемые факторы: Wi-Fi/RSSI/PHY/ретраи, RTT и реально
используемое TCP-окно, затем UDP без TCP.

## Короткий RAM-only замер после async FAT, 2026-11-13

Без смены firmware/частоты/кешей, без SD, текущий Wi-Fi сервер.
262144 B: download 2.649 s / 96.64 KiB/s, upload 3.939 s / 64.98 KiB/s.
Шаблон download и received count upload проверены.

Device GET: 2.611247 s; RX polls34465, empty34247 (99.37%);
34731 register reads /1.512515 s (57.9% elapsed); RX CMD53 33.347 ms,
TX CMD53 59.135 ms; credit stalls9 /9.471 ms.
Device POST:3.899494 s; RX polls52909, empty52665 (99.54%);
53153 register reads /2.327295 s (59.7% elapsed); RX CMD53 62.788 ms,
TX CMD53 29.067 ms; credit stalls0.

Это стоимость активного опроса, а не доказательство, что он один ограничивает
скорость: в паузах ожидания TCP/радио код также крутит polling. RX/TX CMD53 суммарно
около92 ms в каждом направлении. Нехватка вычислительной мощности одного ядра
этим тестом не установлена; второе ядро не устранит сериализацию SDIO.
Дальше нужен TCP capture на ноуте (retransmits, ACK gaps, window), затем
сопоставление с заполнением TX queue и временем C6 events. tcpdump/dumpcap
на момент теста не установлены. Логи: /tmp/p4-asyncfs-server.log, запросы14/15.

## TCP capture RAM-only, 2026-11-13

`target/network-capture/p4-tcp-test.pcap` (ignored build artifact), tcpdump на
wlp0s20f3, snaplen256, только board TCP80. 868 packets, kernel drops0.
262144 B: download7.053 s /36.30 KiB/s; upload3.805 s /67.28 KiB/s.
Оба направления проверены, firmware не менялась.

Download: пять пауз без TCP-пакетов по1.011/0.976/0.904/1.012/0.925 s,
суммарно4.828 s (около68% времени запроса). Перед каждой ноут отправляет ACK
с SACK: поздние диапазоны приняты, в потоке есть дырка. После паузы борда
передаёт недостающий диапазон, cumulative ACK прыгает вперёд. Например,
ACK32769 + SACK{44125:48342}{33581:43801}; через1.011s seq32769:34229,
после него ACK43801. Это восстановление пропусков после ожидания, не SD I/O.
В обе стороны видны повторно переданные/перекрывающиеся sequence ranges.
Нулевых объявленных окон нет. Окно ноутбука scale10, сырые33 означают33792B,
а не33B. ACK новых download-пакетов ноут обычно отправляет сразу.
Capture на ноуте не показывает, где именно пропали данные/ACK: P4/SDIO/C6/радио.
GRO может объединять входные сегменты; простой счётчик перекрытий не является
точным числом потерянных Ethernet-кадров.

Вывод: большие TCP recovery pauses реально ограничивают download в этом прогоне;
причина потерь/переупорядочивания и отсутствие быстрого восстановления ещё не
установлены. Проверять доставку SACK/duplicate ACK до smoltcp, fast retransmit,
порядок/целостность TX SDIO, потери в C6. Второе ядро пока не обосновано.
Human-readable download trace и анализатор сохранены рядом с pcap.

### Проверка условия fast retransmit в smoltcp 0.14

Локальный `socket/tcp.rs`:2069 вычисляет is_window_update как любое отличие
new_remote_win_len от remote_win_len. На2095..2140 duplicate ACK учитывается
только при !is_window_update; иначе dup counter сбрасывается. Fast retransmit
включается после трёх таких duplicate ACK. Входящие sack_ranges здесь не
используются для самостоятельного запуска восстановления.

В capture Linux меняет receive window одновременно с ACK/SACK:
ACK74191:11 ACK,10 SACK,10 смен window;
ACK94916:6 ACK,5 SACK,5 смен window;
ACK175201:18 ACK,17 SACK,17 смен window;
ACK238604:15 ACK,14 SACK,14 смен window.
Во всех четырёх сериях максимальная серия повторных ACK с неизменным окном=0.
Таким образом, если эти ACK дошли до P4, текущий код каждый раз сбрасывает
счётчик и не включает fast retransmit, несмотря на явные SACK-дыры; остаётся RTO.
Это конкретная согласующаяся с capture причина секундных пауз, но не причина
исходных пропусков и не доказательство доставки каждого ACK до P4.
Следующая проверка: host regression/replay одинаковых ACK с растущими SACK и
изменяемым окном, сравнить с неизменным окном; затем осмысленный SACK-aware fix
и hardware A/B. Простое снятие !is_window_update может вызывать ложный retransmit
на чистых обновлениях окна. Registry и firmware в этой проверке не изменялись.

### RSSI после TCP capture

Повторный scan на плате: R4-2G-TEST channel1 RSSI−75 dBm;
IoT channel6 −77 dBm; Lesnaya-7 channel9 −78 dBm. Это beacon scan RSSI,
не непрерывный замер во время передачи. После scan восстановлен Wi-Fi web
на R4-2G-TEST. Проверка положения/ориентации платы относительно AP и повтор
RAM-теста при более сильном сигнале позволит отделить влияние радио.
