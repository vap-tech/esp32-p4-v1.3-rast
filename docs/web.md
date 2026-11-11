# Flashbox: HTTP-файлы на SD

С 2026-11-08, собственные TCP/HTTP и FAT32, без Cargo-зависимостей, heap и RTOS.
Обычный stage-2 image. Во время web-run одна задача владеет SD и EMAC.

## Запуск

```sh
./scripts/flash.sh
python3 scripts/display-control.py --icache on --dcache off --web-run 3600
# Открыть адрес из WEB READY в браузере, например http://192.168.1.167/
```

PHY должен согласовать 100/full, карта — иметь FAT32 в первой MBR partition.
Автоформатирования нет. D-cache OFF / L2=0 проверяются до запуска; запрет
cached EMAC не снимался. I-cache разрешён. ESC или Ctrl-C завершает сервер,
останавливает DMA и возвращает USB-консоль; после этого можно запускать снова.
Срок ограничен указанным временем и DHCP lease; renewal/mDNS пока нет.
Остальные USB-команды, touch и media loop во время web-run не обслуживаются.
LCD scanout продолжает работать аппаратно; совместное WAV+HTTP не заявляется.

## API

| Метод | Путь | Результат |
|---|---|---|
| GET / HEAD | `/`, `/index.html` | gzip HTML из Flash, Content-Encoding: gzip |
| GET / HEAD | `/api/list?path=%2F` | chunked JSON `entries` с name/dir/size |
| GET / HEAD | `/fs/DIR/NAME.BIN` | файл, Content-Length, attachment |
| PUT | `/fs/DIR/NAME.BIN` | потоковое create-new, 201 после finish |

Пути percent-decoded, максимум 96 B включая начальный `/`. Имена FAT32 ASCII
8.3, без `.`/`..` компонентов. Имена не обрезаются. Существующий файл — 409,
отсутствующий — 404, неверное имя — 400, полный том — 507, dirty volume — 503.
Создания каталогов, удаления, overwrite/append и LFN пока нет. Modified не
выдаётся: календарь платы пока невалиден, UI показывает прочерк.

PUT требует Content-Length <= u32::MAX, включая нулевую длину. Отсутствующая
длина — 411; Transfer-Encoding (включая chunked upload) — 501; сжатое тело —
415. Дубли Content-Length/Host и недопустимые заголовки отвергаются. Поддержан
Expect: 100-continue; файл открывается до interim response, поэтому конфликт
не требует загрузки тела. Один запрос на TCP connection, Connection: close. Для HTTP/1.0 listing
использует завершение по закрытию соединения вместо chunked.
Потоковый writer держит 512-B edge buffer, файл не буферизуется целиком.
При EOF/reset/таймауте/ESC до полного тела вызывается abort; при SD I/O error
отката нет, том остаётся dirty. Отключение питания всё ещё требует внешнего fsck.
Потерянный ответ после успешного finish не откатывает уже опубликованный файл.

## Память и Flash-assets

`pack-image.py INPUT OUTPUT frontend/index.html` детерминированно gzip-сжимает
актуальную страницу и добавляет отдельный Flash slot по **0x100000**:
16-B header `<8sII>` = `P4WEB001`, длина, CRC32, далее gzip, padding до 4 B.
Максимум asset 64 KiB; перекрытие runtime с asset slot — ошибка сборки.
Старый вызов packer без HTML остаётся доступен для loader tests, но такой образ
не содержит страницы. `image.sh` всегда упаковывает HTML.

Asset не входит в `.upper` и не занимает постоянную SRAM-копию. До запуска
сервера header/CRC проверяются ROM Flash read; отдача идёт кусками по 1024 B
через SRAM workspace. Не используется XIP или параллельная запись Flash.
Большой FF gap увеличивает firmware.bin примерно до 1 MiB; это не размер runtime.
Старые stage-2 измерения 420656 B относятся к образу без HTTP/assets.

TCP: четыре фиксированных sockets, RX по 2048 B и TX по 1460 B, один сегмент
в полёте на socket. Workspace SD 8192 B статический; EMAC по-прежнему занимает
19328 B нижнего DMA pool. Файловые операции обслуживаются по очереди, ожидающие
соединения получают TCP backpressure. Незавершённый заголовок одного соединения
не блокирует готовый запрос другого. Единственный Timer временно заимствуется
через RefCell<Emac>/with_timer для SD; второго небезопасного Timer handle нет.

Ограниченный TCP IPv4: checksum/pseudoheader, MSS negotiation, cumulative/partial
ACK, повторная отправка с RTO, duplicate/overlap handling, zero-window probe,
FIN/RST и 32 tombstones на 120 s для закрытых four-tuples. Tombstones ограничены
по памяти, старейшие вытесняются. RX out-of-order queue/SACK/window scaling,
IP fragmentation и полноценная congestion-control реализация отсутствуют.
Stop-and-wait ограничивает flight одним MSS; это первая LAN-реализация,
не стек для Internet. Socket idle/retries и отсутствие прогресса операции
ограничены примерно 30 s. HTTP header ограничен RX 2048 B (иначе 431).

## Проверки на плате

- Chromium: реальная страница, listing, выбор двух файлов, последовательные PUT,
  refresh и скачивание с проверкой содержимого. Без mock backend.
- 262148 B и 1048580 B PUT/GET совпали побайтно. Последний 1-MiB run
  с Expect: 100-continue: около 340 KB/s upload и 943 KB/s download,
  параллельные 20 ping без потерь. Wi-Fi/маршрутизатор ноутбука включены в замер;
  это не предел SD/EMAC.
- Повторный PUT — 409; HEAD, 404, invalid name/traversal, пустой файл.
- PUT оборван после 3000 из 9000 B: файл не опубликован, последующий create
  работает. Маленькое RX window клиента и пауза чтения 2 s прошли без порчи.
- Незавершённый header на одном socket не блокирует listing на другом.
- Перепрошивка/повторный mount сохранили записанные файлы. Guards/stack проверяются
  после остановки; измерения и скриншот — ignored `target/web/`.

Host tests проверяют TCP retransmit/partial ACK/duplicate/RST/window/wraparound,
строгий HTTP framing/пути/JSON, HTTP поверх FAT32 с короткими частями тела,
abort/conflict/GET/HEAD/list. Python проверяет упаковку assets/CRC/границы.
Протокольные ориентиры: [TCP RFC 9293](https://www.rfc-editor.org/rfc/rfc9293.html),
[HTTP/1.1 RFC 9112](https://www.rfc-editor.org/rfc/rfc9112.html).

Финальная проверка остановки: RX errors=0, TCP retransmit=1 с успешным
восстановлением, DMA pool used=0 после Drop/reset (peak 19328 B), stack peak
12628/16384 B и guard intact. D-cache ON отвергнут с CacheConfiguration, после
возврата D OFF сервер запускается снова. Default payload около 189 KiB,
BSS 42.25 KiB, work 265 KiB; страница gzip 5730 B отдельно во Flash.
