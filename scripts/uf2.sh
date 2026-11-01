#!/usr/bin/env bash
# Build the release firmware and convert it to a UF2 for BOOTSEL drag-and-drop.
#
#     scripts/uf2.sh [pico2-w|pico-w]     (default: pico2-w)
#
# Writes pico-deskflow-bridge-<board>.uf2 in the repo root, for BOOTSEL. The
# Pico 2 W's includes the bootloader, and pico-deskflow-bridge-pico2-w-update.uf2
# is the firmware alone, for updating from the settings page.
set -euo pipefail
board="${1:-pico2-w}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/firmware"
case "$board" in
    pico2-w)
        target=thumbv8m.main-none-eabihf family=rp2350
        cargo build --release
        (cd "$root/bootloader" && cargo build --release)
        elf="target/$target/release/pico-deskflow-bridge"
        python3 "$root/scripts/elf2uf2.py" --family "$family" "$elf" "$root/pico-deskflow-bridge-$board-update.uf2"
        # The state sector held firmware before the bootloader came along.
        python3 "$root/scripts/elf2uf2.py" --family "$family" \
            --also "$root/bootloader/target/$target/release/pico-deskflow-bridge-bootloader" \
            --blank 0x10020000:0x1000 "$elf" "$root/pico-deskflow-bridge-$board.uf2"
        exit 0
        ;;
    pico-w)
        target=thumbv6m-none-eabi family=rp2040
        cargo build --release --target "$target" --no-default-features --features pico-w
        ;;
    *)
        echo "usage: $0 [pico2-w|pico-w]" >&2
        exit 2
        ;;
esac
python3 "$root/scripts/elf2uf2.py" --family "$family" \
    "target/$target/release/pico-deskflow-bridge" "$root/pico-deskflow-bridge-$board.uf2"
