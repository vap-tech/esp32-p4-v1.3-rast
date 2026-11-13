# Внешние исходники для изучения

`reference/` исключён из Git целиком. Эти проекты не подключаются к сборке
и не являются зависимостями нашего HAL.

## devOS для M5Stack Tab5

* [domgrimm/tab5-devos](https://github.com/domgrimm/tab5-devos), MIT.
* Локальная копия: `reference/tab5-devos`.
* Снимок от 2026-11-12: `2cec6ece6d2d43ec21a9b7329d3070e34d75b7ec`.
* Интересующие места: `components/devos_fileshare/` (страница и потоковый
  файловый сервер), `components/devos_storage/` (FatFs/SDMMC),
  `components/bsp_tab5/bsp_tab5.c` (буферы, поворот, cache writeback и VSYNC).

Копия служит справочником. Наша реализация остаётся bare-metal Rust,
собственными HTTP/FAT32 и драйверами; TCP/IP теперь использует smoltcp.

## ESP-Hosted MCU

* [espressif/esp-hosted-mcu](https://github.com/espressif/esp-hosted-mcu).
* Локально: `reference/esp-hosted-mcu`.
* Snapshot: `4e55cc677f1ef14080b717abcc115c2aaf6a52c5`.
* Форматы пакетов, SDIO transport и RPC protobuf schema используются как
  справочник; библиотеки C/RTOS не подключаются. Файлы имеют собственные SPDX
  лицензии, учитывать при прямом переносе исходного кода.


## smoltcp — зависимость сборки

[smoltcp-rs/smoltcp](https://github.com/smoltcp-rs/smoltcp),0BSD, версия0.14.0.
Подключён из crates.io, точные версии зафиксированы Cargo.lock. В отличие от
копий reference/, это используемый Rust-компонент. default-features=false;
Ethernet/IPv4/TCP/Reno/ICMP echo. std/alloc/async не включены.
Транзитивные Rust-зависимости: bitflags, byteorder, cfg-if, managed, heapless,
hash32, stable_deref_trait. Си-компонентов и генерации привязок нет.

SDIO profiling/raw mode сверены с ESP-Hosted: `port/os/idf/src/eh_host_port_sdio.c`,
`host/mcu/eh_host_mcu_transport/src/eh_host_raw_tp_stats.c`,
`coprocessor/eh_cp_core/src/eh_cp_transport_test.c` и INIT/host-capability TLV.
Тестовый producer использует существующую прошивку C6 v3.0.9, без её замены.

## Async FAT — используемый Rust-компонент

MabezDev/embedded-fatfs, MIT, vendored revision
`7961cd4eb64de51818d8cdda991713784fa98366`.
[Происхождение и исправления](../vendor/embedded-fatfs/UPSTREAM.md).
Без default features, с lfn/unicode. embedded-io-async 0.7 и atomic-waker 1.1.2
зафиксированы Cargo.lock; без std/alloc и async runtime.
