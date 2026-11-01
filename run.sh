#!/bin/sh
# Run an OS set up by ./setup.sh:
#   ./run.sh nitros9 [--ice [ADDR]] [--dap [ADDR]] ...   NitrOS-9 Level 1 (6809)
#   ./run.sh cpm68k  [--ice ...] ...                     CP/M-68K 1.3 (68000)
#   ./run.sh fuzix   [--persist] [--tty2] [--ice ...] ...  FUZIX 0.5 (68000)
# Other options go to the emulator (see target/release/rs6809 --help).
set -eu
cd "$(dirname "$0")"
os=${1:-}
[ $# -gt 0 ] && shift
case "$os" in
nitros9) exec sh examples/nitros9.sh "$@" ;;
cpm68k | cpm) exec sh examples/cpm68k.sh "$@" ;;
fuzix) exec sh examples/fuzix68k.sh "$@" ;;
*)
	sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//'
	exit 1
	;;
esac
