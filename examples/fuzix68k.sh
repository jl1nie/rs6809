#!/bin/sh
# FUZIX 0.5 on the Tiny68K-compatible machine (boots from the disk's boot
# sector, logs in as root).
#   sh examples/fuzix68k.sh            console on this terminal (^C to quit)
#   sh examples/fuzix68k.sh --ice      under the ICE: http://127.0.0.1:6809/
# Build first (once): sh scripts/build-m68k-elf.sh; sh scripts/build-fuzix-tiny68k.sh
# Writes go to artifacts/fuzix68k/hd.img (a copy of disk.img; delete it to start again).
set -eu
cd "$(dirname "$0")/.."
F=artifacts/fuzix68k
[ -f $F/disk.img ] || { echo "run scripts/build-m68k-elf.sh and scripts/build-fuzix-tiny68k.sh first" >&2; exit 1; }
[ -f $F/hd.img ] || cp $F/disk.img $F/hd.img
SYMS=
case " $* " in
*" --ice"* | *" --dap"*) SYMS="--symbols $F/fuzix.sym" ;;
esac
exec cargo run --release -- --cpu 68000 --machine tiny68k --os fuzix \
  --disk $F/hd.img --persist-disk --boot-sector \
  --script examples/fuzix68k/boot.script $SYMS "$@"
