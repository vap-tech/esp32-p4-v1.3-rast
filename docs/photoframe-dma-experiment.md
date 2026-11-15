# Незавершённое переключение кадров DW-GDMA

Исследование rev1.3,2026-11-15. Рабочая фоторамка **не использует** этот режим.
`src/hal/dsi.rs`, `src/hal/interrupt.rs`, `jd9165.rs` возвращены к исходному
scanout: DW-GDMA channel index1, SAR0=0x48000080, auto reload src/dst(type5),
постоянный framebuffer. Это отдельно от JPEG/PPA DMA2D и ошибки BLEND RID.

Цель опыта: готовить фото вне экрана и на границе DMA-кадра менять SAR.
Испытывались single block(type0), shadow(type10) и один terminal LLI(type15).
IRQ source24 routed→CLIC19, global INT_EN=1, channel status generation enabled,
propagation completion/errors, CPU code/data в SRAM, D-cache OFF,L2=0.
LLI64B-aligned в SRAM, SAR/DAR64-bit fields, BLOCK_TS=153599, width64,
SAR master1, DAR master0 (DSI0x50105000), terminal/valid markers.
В ISR предполагалось заново публиковать descriptor и включать channel.

Результаты:

- При type0 ожидаемый DMA_TFR_DONE не был замечен.
- Shadow дал SHADOWREG_OR_LLI_INVALID_ERR=0x2000, completion count0.
- LLI не дал DMA_TFR_DONE. После таймаута channel enabled=2, raw status=0x10
  (DST_TRANSCOMP), CFG0=0xf, CFG1=0x1b8a0001. LLP уже1, SRAM descriptor
  BLOCK_TS=0x257ff,CTL0=0x001e1b41,CTL1=0xc40f87c0 в одном из прогонов.
- Не помогли отключение nonposted_lastwrite, IOC_BLKTFR=1, изменение порядка
  controller enable/configuration. Это были гипотезы, а не установленная причина.
- Источник в постоянном framebuffer0x48000080 вместо PSRAM pool также не дал
  completion. Нельзя объяснять сбой только новым адресом PSRAM.
- Пользователь увидел синий экран. Возврат стабильного образа восстановил вывод.
  В автоматическом восстановлении нового режима уверенности нет, он удалён.

Проба index0 была только собрана, на плату не прошивалась. Глобальный reset
DW-GDMA в ходе опыта не делался. В рабочем коде новых IRQ/descriptor storage
не оставлено. Снимки исходников/логи: `target/photoframe-dsi-experimental.rs`,
`target/photoframe-irq-experimental.rs`, `target/photoframe-*.log` (ignored).
Наличие отдельных SAR snapshot не доказывает повторный обход/остановку DMA.

Следующий полезный эксперимент: отдельный минимальный DSI стенд с двумя
константными поверхностями, без JPEG/PPA/SD и без веба. Сначала снять регистры
и completion на штатном IDF DPI driver на этой же rev1.3, затем сравнить index0/1
и порядок запуска. TRM rev1.3, глава5 VDMA,5.5.4–5.5.7 и5.7; локальные
`esp_lcd/dsi/esp_lcd_panel_dpi.c`, `esp_driver_dma/src/dw_gdma.c`, `dw_gdma_ll.h`.
Пока этот контроль не пройден, подключать этот опыт к фоторамке не следует.
Текущая проверенная реализация: [photoframe.md](photoframe.md).
