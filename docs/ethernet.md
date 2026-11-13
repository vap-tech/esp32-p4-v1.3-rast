# Ethernet bring-up: IP101, RMII, DHCP и ping

Bare-metal Rust, без зависимостей, RTOS, allocator и IRQ. `hal::ethernet`
работает с собственным DMA MAC, не занимает каналы GDMA. Распиновка и источник
схемы записаны в [board.md](../board.md#камера-rtc-и-ethernet).

## Запуск

```sh
./scripts/image.sh
espflash write-bin --port /dev/ttyACM0 0x2000 target/firmware.bin
python3 scripts/display-control.py --icache on --dcache off --eth-probe
python3 scripts/display-control.py --eth-run 120
# В другом терминале, адрес взять из ETH DHCP BOUND:
ping 192.168.1.167
```

Консоль: `:eth-probe`, `:eth-run N` (1..600 секунд), ESC завершает run.
Ctrl-C в управляющем скрипте посылает ESC и ждёт подтверждения остановки.
Обычный boot сеть не запускает; после окончания run MAC/DMA остановлены,
PHY сохраняет физический линк. Это ограниченный по времени диагностический
цикл, пока не фоновая служба совместно с touch/audio/SD. Отдельная команда
`:web-run N` поднимает [TCP/HTTP файловый сервер](web.md) поверх того же EMAC.
IPv6, VLAN, фрагментации IP и DHCP renewal/rebinding пока нет.
При истечении DHCP lease run прекращается, адрес дальше не используется.

С 2026-11-08 Ethernet входит в обычный stage-2 образ. Размерный feature eth-lab
удалён; JPEG/SD bench, tone, image/play и форматирование доступны вместе.
Дескрипторы и буферы Ethernet берутся из отдельного нижнего DMA-пула 152 KiB.
[Загрузка и актуальная карта памяти](stage2-boot.md).

## PHY и часы

MDIO scan нашёл один PHY по адресу 1, ID `0243:0c54`, IC Plus IP101 family.
MAC version `00001137`. BMSR читается дважды из-за latch-low link; 100M full
подтверждается завершённым autonegotiation и общей способностью ANAR/ANLPAR.
Run пока требует именно 100/full. PHY reset и vendor-регистры не изменяются.
MDC работает с консервативным CSR / 102. RMII получает внешний 50-MHz clock
на GPIO50 от PHY с собственным 25-MHz кварцем; MPLL не перенастраивается.

Регистры/порядок настройки сверены с локальным ESP-IDF v6.1:
`esp_hal_emac/esp32p4/include/hal/emac_ll.h`, `esp_hal_emac/emac_hal.c`,
`esp_hal_emac/include/hal/emac_hal.h`, `soc/esp32p4/register/hw_ver1`.
У P4 FIFO всего 256 B; RX/TX store-and-forward выключены, threshold 64 B.

## Буферы и владение

Внутренняя арена: 8 RX + 4 TX buffers по 1536 B, 12 enhanced descriptors
с индивидуальным шагом/выравниванием 64 B, guards по 64 B перед/после allocation.
Всего 19328 B. RX цепочка замкнута; TX использует четыре независимых slots,
следующий CPU-owned descriptor останавливает продвижение до новой публикации.
Вся память принадлежит `Emac` до Drop. RX читается только после OWN=CPU;
готовый frame копируется в фиксированный SRAM scratch, затем OWN возвращается.
TX копирует frame в свободный slot, публикует OWN после `fence rw,rw` и ждёт
аппаратного завершения. Hardware добавляет pad/FCS. Контрольные суммы IP/ICMP
считаются программно; RX проверяет FS/LS, длину и error summary.

Пока **обязательны D-cache OFF и L2=0**, проверяются до allocation/старта DMA.
`fence` не заменяет cache writeback. Не заявляем поддержку cached Ethernet DMA.
Drop снимает start RX/TX, ждёт остановки обоих DMA engines, при таймауте
использует MAC software reset с ограниченным ожиданием. Если остановка не
подтверждается, panic не даёт сбросить арену и переиспользовать память.
Guard проверяется после остановки. Любой выход через `?` также проходит Drop.

## Минимальная обработка пакетов

Лабораторный locally administered MAC `02:f1:b2:d3:81:00` фиксирован для этой
платы; это не чтение factory MAC из eFuse, клонировать адрес на другие платы нельзя.
MAC принимает собственный unicast и broadcast, promiscuous mode выключен.
DHCP DISCOVER/REQUEST содержит client ID и hostname `p4-rust`, запрашивает
broadcast response. Ответы проверяются по xid, chaddr, cookie, размеру и options;
ACK должен соответствовать выбранным offer/server, lifetime должен быть ненулевым.
IPv4 header checksum и nonzero UDP checksum проверяются. IP fragments отвергаются.
После ACK отвечает на ARP request своего адреса и корректный ICMP echo request,
сохраняя identifier/sequence/payload. В eth-run TCP/UDP endpoints не открываются;
web-run дополнительно слушает TCP/80.

Первый TX с одним descriptor, замкнутым на себя, дал реальные DUP ping (3 дубля
на 4 запросах). После перехода на 4 TX slots DHCP стал давать одну пару
DISCOVER/REQUEST, а 100 обычных + 100 MTU ping прошли без потерь и дублей.
Это подтверждение исправления воспроизведения в тесте; точный механизм
аппаратной предвыборки однодескрипторного кольца отдельно не доказан.

Получен DHCP address `192.168.1.167/24`, gateway/server `192.168.1.1`.
Ноутбук находился в другой подсети `192.168.50.0/24`, ping прошёл через маршрутизатор;
его RTT включает Wi-Fi/маршрутизацию и не является измерением задержки MAC.
Первый полный 120-s run: RX=207, TX=204, RX errors=0, ARP replies=2,
echo replies=200, missed/overflow counter=0; stack peak 4408/16384 B, guard intact.

Финальный образ с guards и учётом lifetime lease: 1000 обычных ping и отдельный
повторный старт с 100 MTU ping (1472 B payload, IP total 1500 B) + 100 ping
размеров 0/1/17/128/512 B прошли без потерь и DUP. RX errors/missed counter=0,
guards проверены после stop, stack peak 4392/16384 B. Повторный run после
`Err(CacheEnabled)` тоже прошёл. После остановки Ethernet Z заново смонтировал
SD и прочитал CONFIG.TXT, hash `ce6bb87b` совпал. Неудачная попытка MTU ping
в `final-ping-mtu.log` была уже после окончания 45-s окна; успешный повтор —
`final-ping-mtu-restarted.log`, не смешивать эти результаты.

47 host tests прошли (2 ignored). Host tests проверяют RFC checksum vector, odd payload, DHCP bounds/client/xid,
malformed options, nonzero UDP checksum, ARP target, ICMP options/payload,
ошибочные checksums и fragments. Логи: `target/ethernet/` (ignored).

## Контрольный RAM HTTP тест через коммутатор

Та же firmware и TCP/HTTP, без SD; DHCP192.168.1.167. Борда подключена RJ45,
ноут остаётся Wi-Fi192.168.50.97, маршрут через192.168.50.1.
1 MiB: download1.103 s /928.8 KiB/s, upload0.510 s /2006.9 KiB/s.
4 MiB: download2.047 s /2001.0 KiB/s, upload2.085 s /1964.7 KiB/s.
Download сверён побайтно; upload проверяет received count.
Capture7949 packets, kernel drops0; максимальный интервал между всеми пакетами
9.11 ms, секундных пауз предыдущего C6-прогона нет.
Артефакты: target/network-capture/p4-wired-tcp.pcap и p4-wired-tcp.txt.

Это подтверждает способность одного HP core и текущего стека выдавать около
2 MiB/s в RAM HTTP при другом сетевом тракте. Не измеряет потолок Ethernet:
ноут по-прежнему беспроводной, присутствует routing. Отсутствие секундных пауз
без C6 указывает на важность C6/SDIO/радио-тракта, но не локализует исходные
пропуски и не отменяет найденное условие fast retransmit/window-update в smoltcp.
Wi-Fi и Ethernet HTTP пока запускаются по очереди, не одновременно.

### Ноут и борда на одном проводном сегменте

Ноут enp0s31f6=192.168.1.183/24, Wi-Fi DOWN, прямой route до192.168.1.167.
Без изменения firmware: три RAM HTTP теста по4 MiB на направление:
- download2.082/2.057/2.058 s,1967.0/1991.2/1990.5 KiB/s;
- upload1.587/1.582/1.588 s,2581.1/2588.7/2579.1 KiB/s.
Download проверен побайтно, upload поreceived count.
Capture24924 packets, kernel drops0, файл target/network-capture/p4-all-wired.pcap.
Перекрывающихся data sequence ranges в этих шести соединениях не обнаружено;
GRO/TSO могут объединять наблюдаемые сегменты, поэтому это не счётчик PHY frames.
В download ACK delay на ноуте median0.03–0.04 ms,max0.79 ms, zero-window нет.
В каждом upload у борды681 объявление window0; advertised RX до6144B.
ACK delay upload median1.85–1.87 ms, max2.52 ms. Это регулярная backpressure
при заполнении RX, а не секундные потери/восстановление как в C6-прогоне.
Следующее измерение: RX consumption/poll budgets, bytewise copies и занятость CPU;
для download дополнительно время внутри Emac::transmit, который сейчас ждёт
завершения descriptor после каждого кадра, несмотря на TX ring. Причина потолка
скорости пока не доказана; конфигурацию/алгоритмы в этом тесте не меняли.

### Профиль EMAC и I-cache A/B

Добавлены агрегированные counters; вывод только после RAM net-test.
EMAC array: rx_calls,rx_frames,rx_copy_us,tx_frames,tx_copy_us,tx_wait_us.
Poll tuple: calls,total_us,read_empty,write_full. Времена вложенные: poll включает
EMAC copy/wait; суммировать их как независимые стадии нельзя. AtomicU32 counters
предназначены для коротких прогонов; переполнение при долгом накоплении допустимо.

При холодном старте L1 I/D bypass; предыдущие проводные прогоны стартовали без
явного icache on. Профильная сборка, I OFF:4 MiB download1856–1865 KiB/s,
upload2380–2401 KiB/s. Пример GET2.195189s:TX copy673577us,wait364316us,
RX copy33449us,poll1920444us. POST1.716077s:RX copy677439us,
TX copy12429us,wait13425us,poll1389600us. write_full=0.
Counters снизили скорость относительно прежних1967–1991/2579–2589KiB/s;
это инструментированная, а не бесплатная диагностика.

Та же профильная firmware, только icache on: register readback l1_bypass=18
(decimal=0x12), D bypass остаётся, L2 size0,PSRAM uncached.
Три4MiB GET:1.166/1.159/1.160s,3513.3/3533.4/3531.7KiB/s.
Три POST:0.809/0.808/0.797s,5060.3/5071.7/5139.4KiB/s.
GET1.158466s:TX copy221002us(19.1%),wait362199us(31.3%),RX copy12041us;
poll1022056us,write_full0. POST0.795702s:RX copy222531us(28.0%),
TX copy4619us,wait12449us,poll638756us,read_empty11447,write_full0.
Copy bytewise volatile, по двум сторонам; это отдельно от других TCP/HTTP copies.
RX часто временно пуст во время polling; один этот счётчик не доказывает CPU idle.

I-cache остаётся runtime-настройкой, при reset снова bypass. Default boot policy
в этой работе не изменена. Рекомендация по результатам профиля: широкое копирование
и асинхронная публикация TX ring с проверкой освобождённых descriptor/errors;
оба изменения ещё не реализованы. Глобальный D-cache для EMAC по-прежнему запрещён.
Логи /tmp/p4-eth-prof-live.log и /tmp/p4-eth-prof-icache.log сохранены также
в target/network-capture; новая firmware содержит counters. Host86 pass,
Clippy all-features clean; memory link guards сохранены, payload411200B.

### Исправление: word copy и TX pipeline

`Emac` копирует DMA-сторону 32-bit volatile words, CPU-срезы могут быть
невыровнены; 1–3 хвостовых байта обрабатываются отдельно без выхода за кадр.
Host-тест проверяет разные длины/смещения, содержимое и обе guard-области.
TX публикует OWN и возвращается после копирования в собственный ring buffer,
не ожидая именно этого кадра. Ждёт (до10ms) только перед повторным использованием
занятого slot. Pending mask учитывает все четыре descriptors; receive/transmit
снимают завершения и проверяют error summary. Ошибка TX отличается от RX Frame,
передаётся как transport failure; при остановке DMA storage удерживается до
подтверждения остановки как прежде. D-cache ON по-прежнему не разрешён.

Проводной `web-run` теперь сам включает I-cache после проверки DMA cache policy;
D-cache/L2 не переключаются. I-cache остаётся включённым после возврата в консоль,
до reset или явного icache off. Начальная boot policy по-прежнему bypass.
TX token содержит временный CPU workspace: после возврата driver владеет своей
копией, а не удерживает borrowed slice smoltcp до окончания DMA.

Та же профильная сборка с исправлением, 3x4MiB:
GET0.771/0.763/0.762s,5312.4/5371.7/5373.7KiB/s;
POST0.720/0.724/0.715s,5691.1/5657.4/5731.5KiB/s.
Пример GET760793us:TX copy180636us,TX wait5733us (вместо362199us),
RX copy8266us,poll624580us,write_full0.
POST713123us:RX copy140314us,TX copy4173us,wait2447us,poll556193us.

Capture24545 packets,kernel drops0; overlapping TCP data ranges не обнаружены,
паузы между data-пакетами >20ms в этих соединениях отсутствуют.
RX advertised window0 при POST всё ещё регулярно встречается (681 раз/запрос),
но время возобновления уменьшилось: ACK delay median~0.88ms,max1.20ms.
Режим polling и приложение всё ещё можно оптимизировать; это не line-rate.
12 reconnect,3 simultaneous clients,GET/RST,truncated RAM POST,SD create/read
32772B,HOME.JPG read и cleanup прошли; root до/после совпал.
Host87 tests pass,Clippy clean,payload411504B.
Capture и логи: target/network-capture/p4-eth-ring.pcap и p4-eth-ring-live.log.
31 HTTP requests completed,rx_errors0;stack44144/65536B,guard intact.
