#!/usr/bin/env bash
set -euo pipefail

workspace_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
elf_path="${1:-$workspace_dir/target/xtensa-esp32s3-espidf/debug/ble-logger}"
image_path="$(dirname "$elf_path")/ble-logger-flash.bin"
port="${BLELOG_PORT:-/dev/ttyACM0}"
before="${BLELOG_BEFORE:-usb-reset}"

"$workspace_dir/scripts/make-firmware-image.sh" "$elf_path" "$image_path"

exec espflash write-bin \
    --skip-update-check \
    --chip esp32s3 \
    --port "$port" \
    --before "$before" \
    0x0 "$image_path"
