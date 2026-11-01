#!/bin/sh
# CP/M-68K 1.3 on the Tiny68K-compatible machine.
#   sh examples/cpm68k.sh            console on this terminal (^C to quit)
#   sh examples/cpm68k.sh --ice      under the ICE: http://127.0.0.1:6809/
# The first run fetches the distribution and makes artifacts/cpm68k/a.img
# (writes to the disk are kept; delete it to start again).
set -eu
cd "$(dirname "$0")/.."
D=artifacts/cpm68k/dist/DISK8
[ -f $D/CPM15000.SR ] || python3 scripts/fetch-cpm68k.py
[ -f artifacts/cpm68k/a.img ] || python3 scripts/cpm68k-disk.py dist artifacts/cpm68k/a.img examples/cpm68k/hello.c
# Symbols for the ICE: BIOS source lines and the CCP/BDOS names.
SYMS=
case " $* " in
*" --ice"* | *" --dap"*) SYMS="--symbols examples/cpm68k/bios.lst --symbols $D/CPM15000.MAP" ;;
esac
exec sh scripts/rs6809 --cpu 68000 --machine tiny68k \
  --bin $D/CPM15000.SR --bin examples/cpm68k/bios.sr --pc 0x15000 \
  --disk artifacts/cpm68k/a.img --persist-disk $SYMS "$@"
