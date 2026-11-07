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
цикл, пока не фоновая служба совместно с touch/audio/SD. TCP, HTTP, файлового
сервера, IPv6, VLAN, фрагментации IP и DHCP renewal/rebinding пока нет.
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
сохраняя identifier/sequence/payload. TCP/UDP endpoints не открываются.

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
