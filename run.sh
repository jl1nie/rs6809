#!/bin/sh
# Run an OS set up by ./setup.sh:
#   ./run.sh nitros9 [--ice [ADDR]] [--dap [ADDR]] ...   NitrOS-9 Level 1 (6809)
#   ./run.sh cpm68k  [--ice ...] ...                     CP/M-68K 1.3 (68000)
#   ./run.sh fuzix   [--persist] [--tty2] [--ice ...] ...  FUZIX 0.5 (68000)
#   ./run.sh fpga6809|fpga68k DEVICE [...]   the ICE on rs6809-fpga's board (Tang Nano 20K)
# Other options go to the emulator (see target/release/rs6809 --help).
set -eu
cd "$(dirname "$0")"
os=${1:-}
[ $# -gt 0 ] && shift
case "$os" in
nitros9) exec sh examples/nitros9.sh "$@" ;;
cpm68k | cpm) exec sh examples/cpm68k.sh "$@" ;;
fuzix) exec sh examples/fuzix68k.sh "$@" ;;
fpga6809)
	dev=${1:?the board's serial port, e.g. /dev/ttyUSB1 (or tcp:HOST:PORT)}
	shift
	exec sh scripts/rs6809 --fpga "$dev" --ice "$@"
	;;
fpga68k)
	dev=${1:?the board's serial port, e.g. /dev/ttyUSB1 (or tcp:HOST:PORT)}
	shift
	F=artifacts/fuzix68k-tn20k
	SYMS=
	[ -f $F/fuzix.sym ] && SYMS="--symbols $F/fuzix.sym"
	[ -d $F/usersyms ] && SYMS="$SYMS --symbols $F/usersyms"
	exec sh scripts/rs6809 --cpu 68000 --fpga "$dev" --os fuzix $SYMS --ice "$@"
	;;
*)
	sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'
	exit 1
	;;
esac
