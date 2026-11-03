#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
./scripts/image.sh
# ROM loads our image directly from the second-stage boot address.
exec espflash write-bin --port "${1:-/dev/ttyACM0}" 0x2000 target/firmware.bin
