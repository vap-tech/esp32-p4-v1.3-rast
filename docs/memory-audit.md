# Аудит SRAM и достижимости кода — 2026-11-13

Аудит выполнен для текущей рабочей версии с FileTable и параллельными файловыми
HTTP transfers, до выделения каких-либо новых feature flags. Рабочие исходники
runtime и firmware не изменялись, плата не перепрошивалась. Отчёт относится к
uncommitted multi-file версии, а не только к последнему git commit.

> После этого аудита features уже реализованы: [build-flavors.md](build-flavors.md).
> Размеры и пробы ниже относятся к полной версии до разделения.

## Что именно измерено

Rust1.99.1, release=z, smoltcp=s, LTO, codegen-units1, target riscv32imafc.
`scripts/audit-memory.py` делает private temporary snapshot src/vendor/config,
меняет только тела выбранных command handlers, собирает offline в отдельном
`target/memory-audit/build`, читает linker symbols через nm, удаляет snapshot.
Оригинальный ELF проверяется SHA256 до/после. Командные parser/help tables в
пробах сохранены. Пробы не создают flash image и никогда не вызывают flasher.

Результаты: `target/memory-audit/report.json`, полные build logs и symbols.txt.
Baseline snapshot точно воспроизвёл размеры рабочего ELF. Это измерение размера
и статической достижимости, не времени последнего вызова. Счётчиков «давно не
используется» у нас нет; частоту использования по исходникам определить нельзя.
Линкер уже удаляет недостижимый код, но доступный console handler удерживает свои
зависимости независимо от того, запускали ли его недавно.

## Точная базовая раскладка

| Регион | Байт | KiB |
|---|---:|---:|
| Верхний runtime text |327854|320.17|
| Его rodata |42422|41.43|
| Initialized data |1456|1.42|
| Итого загружаемый payload |371732|363.02|
| Runtime BSS |4608|4.50|
| Свободная верхняя work arena |82368|80.44|
| Runtime stack, резерв |65536|64.00|
| Нижняя scoped DMA arena, резерв |118784|116.00|
| Нижние fixed DMA workspaces, резерв |36864|36.00|
| В fixed реально занято |34816|34.00|
| Boot stack/legacy CPU scratch |8192|8.00|
| ROM-reserved |98304|96.00|
| SPM code, отдельно от HP SRAM |1650|1.61|

Пять file slots занимают4408B внутри runtime stack, не ещё пять8-KiB buffers.
Измеренный stack peak52868/65536B оставляет примерно12.4KiB резерва; уменьшать
стек ради арены сейчас необоснованно. Веб по Ethernet занимает117632B нижнего
pool из118784B; вне web-run временные leases освобождаются. Fixed workspaces:
Wi-Fi RX16KiB/TX2KiB, I2S8KiB, SD8KiB. Они нужны рабочим драйверам.
ROM-reserved96KiB не считаются доказанно свободной памятью.

HTML/gzip уже хранится отдельным Flash asset, framebuffer находится в PSRAM.
Корпусные модели, reference/, old/, host tests и их JPEG fixtures не входят в
runtime. Feature audio-lab отключён в default. Они не объясняют расход SRAM.

## Отдельные пробные сборки

Каждая строка сравнивается с полной baseline, кроме явно обозначенной совокупной.
Дельты **не складываются**: LTO, inlining и общие зависимости меняются совместно.
Сохранённый parser/help немного ограничивает экономию в этой методике.

| Что отключено только в пробе | Payload KiB | Освободилось payload KiB | Свободная work KiB |
|---|---:|---:|---:|
| Ничего |363.02|0|80.44|
| JPEG benchmarks/profile, 3 handler |355.36|7.66|88.12|
| Memory/PSRAM experiments, 4 handler |359.12|3.90|84.38|
| SD read/write/DMA tests, 5 handler |355.79|7.23|87.69|
| Остальные labs, 6 handler |358.07|4.95|85.38|
| Все перечисленные labs совместно |336.38|26.64|107.06|
| Только Wi-Fi web specialization |325.82|37.20|117.62|
| Все labs + старые SD console helpers |328.15|34.87|115.31|
| Labs + SD helpers + Wi-Fi web |288.59|74.43|154.88|

Верхняя work gain округляется linker alignment, поэтому может немного отличаться
от payload delta. BSS и fixed DMA во всех этих пробах остались прежнего размера.
В no-labs SPM также уменьшился на254B, но это не добавляет HP work arena.

### Кандидат №1: лабораторные команды вне default

JPEG: jpeg benchmark idct, jpeg profile, jpeg benchmark output.
Memory: memory benchmark, memory test, memory psram-test, system cache l2-probe.
SD: sd test dma, sd test multi-write, sd test write, sd benchmark read, sd probe.
Остальное: system irq test, wifi raw, memory arbitration status/bench,
audio test loopback/underrun/control/repeat, system test mixed.

Это исследовательские/стрессовые точки входа. Результаты уже задокументированы;
они подходят для отдельного diagnostics feature, доступного по необходимости.
Проба сохраняет рабочие web, image/play/record/alarm, RTC, camera, форматирование,
clock/memory status, cache controls, Wi-Fi connect/scan и драйверы. Генераторы и
тесты display/controller, audio tone в этой группе не отключались.

При реальной реализации feature нужно фильтровать COMMANDS/ROUTES и help вместе
с dispatch. Просто скрыть вызовы и оставить объявления пользователю нельзя.
В аудите маршруты намеренно сохранены ради сопоставимой пробы; это не готовый
production/lab build switch. Дополнительную экономию полного gating не измеряли.

### Кандидат №2: общий web/FS backend для Ethernet и C6

`Card<P>` зависит от типа сетевого порта, хотя storage нужен лишь доступ к timer
и уведомлению о занятой SDMMC-шине. Из-за P дважды специализируются FileTable,
FAT IO, mount, HTTP transfers и значительная часть server/async code.

Примеры реальных отдельных символов, bytes на каждую специализацию:
- runtime::run(FAT mount):5604 +5604;
- runtime::run(asyncfs service):8016 +8016;
- runtime::run(file start):2672 +2672;
- runtime::run(file step):2116 +2116;
- web_server::serve и Network::tick также имеют Wi-Fi/EMAC экземпляры.

Некоторые нижние функции LTO уже объединил; raw grep по имени WifiPort не даёт
честного размера радио, так как это имя входит в generic args всей цепочки.
37.20KiB получены **удалением одной web точки входа**, а не уже реализованным
объединением. Wi-Fi connect/probe/SDIO и fixed RX/TX при этом остались.
Это ориентир стоимости второго пути. Нельзя обещать ровно такую же экономию
после рефакторинга с сохранением обоих интерфейсов: потребуются runtime dispatch
и повторная проверка скорости/кеша/SDIO arbitration.

Рекомендуемый путь: отделить storage/timer owner от network generic либо выбрать
единый enum порта на внешней границе, чтобы filesystem имел один concrete тип.
Сохранить оба транспорта. Затем сравнить отдельной сборкой и RAM/SD тестами.

### Кандидат №3: две рабочие реализации FAT

Старый storage::fat32 всё ещё используется обычными image/play, RTC alarm audio,
recorder и SD console helpers. Новый embedded-fatfs/FileTable — вебом.
Среди именованных функций непосредственно legacy FAT около7.7KiB машинного кода;
часть чтения/ошибок/обёрток встроена в callers. Это нижняя оценка, не обещанная
экономия удаления: equivalent implementation в новом пути тоже занимает код.

Удалять старый FAT прямо сейчас нельзя: звук, запись и будильник потеряют storage.
Сначала перенести consumers на общий service, проверить непрерывное аудио и
дописывание, затем удалить дубликат. Старые sd config-create/list/capabilities
плюс labs дают34.87KiB в пробе, но capabilities полезны для bring-up, а list/config
разумнее перевести на новый слой; это не список функций для безусловного удаления.

## Важная находка: Matrix остаётся рабочим JPEG default

`media::jpeg::Profile::disabled()` выбирает Kind::Matrix. Обычный decode вызывает
именно disabled profile, а image-команда вызывает decode. Следовательно обычная
картинка сейчас использует матричный IDCT. Loeffler выбирается явно в benchmarks
и mixed stress. Это подтверждено исходниками; matrix_dense остался даже в
совокупной урезанной пробе, где benchmarks недостижимы.

Поэтому Matrix нельзя сейчас объявить только host reference и удалить вместе
с тестами. Следующий отдельный фикс: согласовать production IDCT default с уже
проверенным Loeffler, проверить пиксели/время и лишь затем спрятать Matrix за
cfg(test)/diagnostics. В рамках аудита алгоритм/изображение не менялись.

## Порядок действий

1. Вынести экспериментальные command handlers в diagnostics feature: измеренные
   26.6KiB payload, work примерно80→107KiB, без отказа от рабочих возможностей.
2. Объединить generic web/FS chain, сохранив C6 и Ethernet. Здесь существенный
   потенциал, но37.2KiB — ориентир пробы, не готовый результат рефакторинга.
3. Исправить production JPEG selection и отделить эталон/профилирование.
4. Перенести media/recorder на единый FS owner и убрать старый FAT после проверки.
5. После уборки повторить карту. Перенос кода/данных во Flash/PSRAM и overlays
   обсуждать уже с обновлённым остатком, без динамического «вытеснения» наугад.

Контрольный урезанный образ с154.9KiB free отключал Wi-Fi web и часть SD console,
поэтому не является рекомендацией прошивать его или обещанием такого размера
при сохранении всех функций. Все реальные рабочие функции в текущей прошивке
остаются на месте; аудит создаёт только этот отчёт и воспроизводимый скрипт.
