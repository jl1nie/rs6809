#!/bin/sh
# Build FUZIX for Tiny68K (reference/FUZIX) with the m68k-elf toolchain from
# scripts/build-m68k-elf.sh, and copy the results to artifacts/fuzix68k:
#   disk.img     the IDE disk (partition table, loader at LBA 1, kernel at
#                LBA 2, file system at LBA 2048; byte-swapped like the board)
#   fuzix.map    kernel link map (symbols for the ICE and the FUZIX monitor)
#   fuzix.elf    kernel with symbols
#   fuzix.sym    its symbols as nm output (for the ICE: --symbols)
#   offsets.txt  structure offsets for the FUZIX monitor (scripts/fuzix-offsets.c)
set -eu
cd "$(dirname "$0")/.."
ROOT=$PWD
FUZIX=$ROOT/reference/FUZIX
OUT=$ROOT/artifacts/fuzix68k
TOOLS=$ROOT/artifacts/tools/m68k-elf/bin
[ -x "$TOOLS/m68k-elf-gcc" ] || { echo "run scripts/build-m68k-elf.sh first" >&2; exit 1; }
[ -d "$FUZIX/Kernel" ] || { echo "clone FUZIX into reference/FUZIX" >&2; exit 1; }
export PATH="$TOOLS:$PATH"
mkdir -p "$OUT"
# gcc 10.3 -Os mis-sizes a switch table in as09's as.c ("value -684 out of
# range", the problem FUZIX's README.68000.md reports for gcc 11): build it
# without jump tables once the C library is there.
(cd "$FUZIX" && make TARGET=tiny68k -j"$(nproc)" libs)
app() { # dir [make args]
	d=$1
	shift
	(cd "$FUZIX/Applications/$d" &&
		FUZIX_ROOT=$FUZIX TARGET=tiny68k CPU=68000 USERCPU=68000 make -f Makefile.68000 "$@")
}
app as09 COPT=-fno-jump-tables
# MWC/cmd runs byacc for two grammars into the same y.tab.c, which a
# parallel make races on (one grammar's output ends up as the other's .c).
app MWC/cmd
(cd "$FUZIX" && make TARGET=tiny68k -j"$(nproc)" diskimage)
cp "$FUZIX/Images/tiny68k/disk.img" "$OUT/disk.img"
cp "$FUZIX/Kernel/fuzix.map" "$OUT/fuzix.map"
cp "$FUZIX/Kernel/platform/platform-tiny68k/fuzix.elf" "$OUT/fuzix.elf"
# Kernel symbols in nm form, which the ICE reads (--symbols).
m68k-elf-nm -n "$OUT/fuzix.elf" > "$OUT/fuzix.sym"
# Structure offsets the FUZIX monitor uses, from this kernel's headers.
K=$FUZIX/Kernel
m68k-elf-gcc -S -o - -m68000 -Os -fno-builtin -I"$K/cpu-68000" -I"$K/platform/platform-tiny68k" \
	-I"$K/include" "$ROOT/scripts/fuzix-offsets.c" | awk '$1 == ".equ" { sub(",", "", $2); print $2, $3 }' > "$OUT/offsets.txt"
echo "FUZIX $(git -C "$FUZIX" log -1 --format=%h) in ${OUT#$ROOT/}"
