# Постоянный HP1 worker — 2026-11-14

В штатной сборке `hp-worker` включён по умолчанию. HP1 живёт одновременно
с web/SD/LCD/часами. Без задач спит в WFI; вычислительная нагрузка по
умолчанию выключена. Ни RTOS, ни потокового общего heap, ни новых зависимостей.
`--no-default-features` оставляет однопроцессорный вариант. Boot probes
`multicore-lab` и экспериментальная раскладка `l2-128` отключают runtime worker.

## Роли и память

HP0 — единственный владелец SDMMC, Ethernet, I²C/RTC/touch, LCD, I²S и аллокаторов.
HP1 получает только числовые задания в ограниченной SPSC очереди и возвращает
результаты другой очередью. Его собственный стек 8 КиБ, очереди, startup ports
и status используют некешируемый alias SRAM `0x8ff...` на обоих ядрах.
Никаких ссылок на Arena/RefCell или DMA-буферы HP0 второму ядру не передаём.

Реализация: `src/worker.rs`, `src/worker/hardware.rs`, `src/intercore.rs`.
HP0 controller имеет одного foreground владельца; короткий nonblocking
reentrancy guard исключает перекрывающиеся borrow при вложенном housekeeping.
ISR никогда не обращается к controller, очередям или драйверам.

Сейчас единственный тип задания — integer xorshift, 4096 итераций.
Это проверочная нагрузка, не модель и не аудиообработчик. HP0 один раз
вычисляет эталон при старте, затем проверяет ID/результат без повторения
тяжёлого вычисления. Не более 8 заданий outstanding, backpressure не теряет
владение. При `load off` новые задания не появляются, старые завершаются.

Рабочая конфигурация: I-cache HP0 и HP1 ON, D-cache OFF, L2=0,
PSRAM uncached. Политика Ethernet DMA по D-cache не ослаблялась.

## Прерывания и основной цикл

FROM_CPU0 → HP1 IRQ18, FROM_CPU1 → HP0 IRQ18. Уведомления — уровни,
очереди остаются источником истины. HP1 обработчик использует только integer
регистры, подтверждает источник и считает IRQ. HP0 штатный trap сохраняет
integer frame, dispatch18 подтверждает источник и будит foreground future.

Scoped SDMMC/SYSTIMER guard сохраняет enable IRQ18, не изменяет его маршрут
и восстанавливает ранее установленный worker IRQ при выходе. Межъядерный
IRQ не занимается SD, сетью, декодированием, аллокациями или SYSTIMER snapshot.

`worker::poll()` вызывается в idle main, cooperative runtime и network tick.
Работа ограничена размером очередей: drain результатов и при включённой
нагрузке пополнение до 8 outstanding. При повторном входе poll возвращает
сразу. Долгие blocking операции в старых диагностических командах всё ещё
могут задерживать пополнение, но HP1 не получает чужую периферию или память.

FP/PIE/HWLOOP в этом worker/ISR пока не используются. Перед их применением
нужно отдельно определить сохранение расширенного состояния.

## Пауза для обслуживания кешей

Обычный WFI в SRAM недостаточен: ядро может продолжить выборку инструкций
или внезапно проснуться во время изменения кешей.

Пауза работает так:

1. HP0 публикует новый control token: нечётный — pause, чётный — resume.
   Каждая команда получает новую generation.
2. HP1 завершает текущее задание/публикацию ответа и вызывает `hpw_pause`.
3. Эта короткая функция полностью в SPM (`.text.entry`), IRQ HP1 выключены,
   stack/рабочая SRAM не читаются и не записываются. Доступ только к status
   через uncached alias и к FROM_CPU MMIO.
4. HP1 публикует PAUSED и точный ack token. Запросы/результаты в очередях
   сохраняются. HP0 считает паузу действительной только при совпадении
   state=PAUSED, нечётного request и request=ack.
5. При resume HP1 выполняет fence.i, публикует RUNNING/ack и возвращается
   из SPM к обработке очереди.

Проверка generation нужна: старый PAUSED при resume-in-flight не является
разрешением на cache maintenance. В host-тесте отдельно проверены stale ack,
resume token и переходные/fault состояния.

HAL cache helpers проверяют это разрешение. Живой worker допускает только
идемпотентный `set_instruction_cache(true)` без invalidation (повторный старт
web); изменение возвращает `WorkerActive`. IRQ HP0 маскируются локально на
время операции. D-cache transition остаётся отдельной SPM функцией, чтобы
проверка разрешения/её frame не попадали внутрь writeback/bypass sequence.

Разрешение со стороны CPU НЕ отменяет требования DMA-когерентности,
размещения и выравнивания. В web не переключаем D-cache: cache-check проверяет
только I1, пока HP1 находится в SPM. Размер L2MEM/stack layout на лету не меняем.

Снимок stack watermark и guard читается только в подтверждённой паузе.
При RUNNING выводится None, чтобы диагностика не читала изменяемый Rust stack.

## Сбросы

В самом начале SPM bootstrap HP0 удерживает HP1 в global reset и отключает
его clock до cache maintenance, BSS zero и загрузки SRAM payload. Только
после собственной инициализации runtime HP0 заново запускает HP1 через ROM.
Это не протокол сохранения заданий при reset: очереди создаются заново.
ROM entry hook очищается после startup handshake.

## Управление

Команды работают как в idle console, так и внутри постоянного web:

```
:system worker status
:system worker load on
:system worker load off
:system worker pause
:system worker resume
:system worker cache-check
```

На ноуте, когда другой USB reader остановлен:

```
python3 scripts/display-control.py --worker status
python3 scripts/display-control.py --worker 'load on'
python3 scripts/display-control.py --worker pause
python3 scripts/display-control.py --worker cache-check
python3 scripts/display-control.py --worker resume
python3 scripts/display-control.py --worker 'load off'
```

`cache-check` при RUNNING должен вернуть `WorkerActive`, при PAUSED —
успешное выключение/включение I1. Данные D-cache/DMA при этом не обслуживаются.
Числовые state: 1 RUNNING, 2 PAUSED, 3 FAULT, 4 STARTING. Ошибка startup —
остановка загрузки с требованием reset. При fault HP1 память не освобождается.

## Проверки

110 host-тестов; Clippy штатного и all-features вариантов.
Сохранённые образы/логи/данные: `target/hp-worker/` (локальные артефакты).

Первая смешанная серия на плате:

- 774005 заданий завершены и проверены, errors=0, trap=0; после load off
  pending=0. HP0 IRQ=774005, HP1 IRQ=613019 (уведомления объединяются).
- Одновременно PUT 64 МиБ на SD и GET с совпадающим SHA-256; файл удалён,
  root listing до/после совпал. PUT 33.208 с, GET 19.012 с.
- Параллельно четыре RAM GET и четыре RAM POST по 4 МиБ.
- 20 pause/resume циклов во время нагрузки и четыре I1 cache-check в паузе.
  Очередь сохранялась, после resume результаты продолжали поступать.
- В паузе stack high-water 80/8192, guard цел.
- Фоновые часы/тач: RTC ошибок 0, максимальный UI gap около 13.3 мс;
  LCD errors0/errors1=0. Это не визуальное доказательство отсутствия tearing.
- Проверен тёплый reset после работы HP1 под нагрузкой; повторный startup HP1
  и автоматический запуск web успешны.

Здесь не было воспроизведения WAV (`starts=0` в audio summary). Совместная
непрерывная аудиопередача с постоянным HP1 требует отдельной проверки.
Также не передавались изменяемые кешируемые payload-буферы и не измерялась
общая производительность ML/DSP. Из этой серии нельзя выводить их скорость.

Финальный default image дополнительно проверен PUT/GET16 МиБ с точным
SHA-256, пятью pause/resume/I1 cache-check циклами и освобождением web ресурсов
при активном worker. GC освободила 5940736 байт PSRAM, open_files=0;
очереди HP1 сохранились. Проверено 63565 результатов, errors=0, pending=0,
stack watermark снова 80/8192. Пользовательские файлы/root listing неизменны.

Финальная штатная сборка также выдержала тёплый reset при `load=true`:
после него HP1 заново READY, web автоматически поднят, нагрузка снова OFF.
Перед warm-reset испытаниями web закрывался через ESC, чтобы завершить
файловые операции; HP1 при этом продолжал нагрузку в main. Обрыв питания
во время записи FAT32 и восстановление потерянных цепочек этим не проверены.
Host utility `--worker status/pause/resume` проверена на финальной сборке;
после последнего reset счётчики стартовали с нуля и нагрузка OFF.
