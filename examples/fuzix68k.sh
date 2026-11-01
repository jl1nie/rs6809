#!/bin/sh
# FUZIX 0.5 on the Tiny68K-compatible machine (boots from the disk's boot
# sector, logs in as root).
#   sh examples/fuzix68k.sh            console on this terminal (^C to quit)
#   sh examples/fuzix68k.sh --ice      under the ICE: http://127.0.0.1:6809/
#   sh examples/fuzix68k.sh --persist  keep writes in artifacts/fuzix68k/hd.img
#                                      (a copy of disk.img); type telinit 6 before quitting
# Build first (once): sh scripts/build-m68k-elf.sh; sh scripts/build-fuzix-tiny68k.sh
# Without --persist each run boots the built disk.img afresh and writes stay
# in memory. With it, quitting without telinit 6 (which unmounts and halts)
# leaves the file system dirty: the next boot checks and repairs it (the boot
# script answers fsck's questions).
set -eu
cd "$(dirname "$0")/.."
F=artifacts/fuzix68k
[ -f $F/disk.img ] || { echo "run scripts/build-m68k-elf.sh and scripts/build-fuzix-tiny68k.sh first" >&2; exit 1; }
DISK="--disk $F/disk.img"
if [ "${1:-}" = "--persist" ]; then
	shift
	[ -f $F/hd.img ] || cp $F/disk.img $F/hd.img
	DISK="--disk $F/hd.img --persist-disk"
fi
SYMS=
case " $* " in
*" --ice"* | *" --dap"*)
	SYMS="--symbols $F/fuzix.sym"
	# Each program's symbols, placed on the processes running it.
	[ -d $F/usersyms ] && SYMS="$SYMS --symbols $F/usersyms"
	;;
esac
exec sh scripts/rs6809 --cpu 68000 --machine tiny68k --os fuzix \
  $DISK --boot-sector \
  --script examples/fuzix68k/boot.script $SYMS "$@"
