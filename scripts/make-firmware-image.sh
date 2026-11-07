#!/usr/bin/env bash
set -euo pipefail

workspace_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
elf_path="${1:-$workspace_dir/target/xtensa-esp32s3-espidf/debug/ble-logger}"
output_path="${2:-$workspace_dir/target/xtensa-esp32s3-espidf/debug/ble-logger-flash.bin}"
artifact_dir="$(dirname "$elf_path")"
idf_dir="$workspace_dir/.embuild/espressif/esp-idf/v5.5.3"

if [[ ! -f "$elf_path" ]]; then
    echo "firmware ELF not found: $elf_path" >&2
    echo "run 'cargo +esp firmware-build' first" >&2
    exit 1
fi

if [[ ! -f "$artifact_dir/bootloader.bin" ]]; then
    echo "firmware bootloader not found: $artifact_dir/bootloader.bin" >&2
    exit 1
fi

esp_python=""
for candidate in "$workspace_dir"/.embuild/espressif/python_env/*/bin/python; do
    if [[ -x "$candidate" ]]; then
        esp_python="$candidate"
        break
    fi
done

if [[ -z "$esp_python" || ! -f "$idf_dir/components/partition_table/gen_esp32part.py" ]]; then
    echo "the pinned ESP-IDF tools are not installed under .embuild" >&2
    echo "run 'cargo +esp firmware-build' first" >&2
    exit 1
fi

image_tmp_dir="$(mktemp -d)"
trap 'rm -rf -- "$image_tmp_dir"' EXIT
partition_bin="$image_tmp_dir/partition-table.bin"
app_bin="$artifact_dir/ble-logger-app.bin"

"$esp_python" "$idf_dir/components/partition_table/gen_esp32part.py" \
    "$workspace_dir/partitions.csv" "$partition_bin"

"$esp_python" -m esptool --chip esp32s3 elf2image \
    --flash_mode qio \
    --flash_freq 80m \
    --flash_size 16MB \
    --output "$app_bin" \
    "$elf_path"

app_size="$(wc -c < "$app_bin")"
if (( app_size > 0x300000 )); then
    echo "application image ($app_size bytes) exceeds the 3 MiB factory partition" >&2
    exit 1
fi

mkdir -p "$(dirname "$output_path")"
"$esp_python" -m esptool --chip esp32s3 merge_bin \
    --output "$output_path" \
    0x0 "$artifact_dir/bootloader.bin" \
    0x8000 "$partition_bin" \
    0x10000 "$app_bin"

echo "firmware image: $output_path ($app_size-byte application in 3 MiB slot)"
