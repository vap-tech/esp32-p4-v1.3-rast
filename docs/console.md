# Console commands

Legacy key reference (all commands remain available):

- :icache on/off
- :memory / :memory-test / :memory-bench
- :image PATH / :jpeg-bench PATH / :jpeg-profile PATH / :play PATH (ESC stops audio)
- +/- backlight5%
- ]/[ digital gain
- }}/{{ analogue gain
- >/< exposure
- = controls
- p quiet 1kHz speaker tone 2s
- ! FAT32 create/verify CONFIG.TXT
- Z FAT32 list/read-only file check
- Y SD capabilities (no speed switch)
- M SDMMC read-only probe
- m SD multi-block read benchmark
- # SD multi-write/verify/restore last 9 sectors
- @ SD write/verify/restore last sector
- Q panel RGB probe
- E gray/RGB check
- W full screenshot
- H thumbnail
- V camera-live
- G camera-test-live
- O camera-stop
- F framebuffer
- S fb-status
- U patterns
- P psram-test
- a camera-info
- A camera-test
- o camera-scene
- ? help
- q status
- B BIST-probe25s
- L scanline-probe
- e error-counter(clear)
- v/h/b patterns
- i/I invert on/off
- x/y/n mirror X/Y/none
- c/C BGR/RGB
- d/D display off/on
- j/J idle on/off
- k/f/N black/white/normal
- s/w sleep/wake
- t/u/T TE vblank/both/off
- l TE line300
- g idle-green
- r/R soft/hard reset
- 6/7 RGB565 at650/750
- 8 RGB888 at750
- z RGB565 at550

DMA soak: `python3 scripts/display-control.py --sd-stress` after framebuffer initialization; I+D ON, L2=0.

Additional diagnostics:

- `:mixed-stress` / `--mixed-stress`: muted generated PCM + 20 JPEG + LCD.
- `:play-stall SATIE.WAV` / `--play-stall`: expected underrun owner error and safe stop.

See [DMA validation](dma.md) for setup and limits.

Cache controls: `:icache on/off`, `:dcache on/off`, `:l2 on/off` (l2-128 build), `:l2-probe`.

Live WAV diagnostic (audio-lab build): `:wav-lab`, `:wav-lab-control`,
`:wav-lab-repeat`, `:wav-lab-stall` with matching script flags.
[Setup and unavailable legacy commands in this build](wav-lab.md).
