#!/bin/sh
# Build FUZIX for Tiny68K (reference/FUZIX) with the m68k-elf toolchain from
# scripts/build-m68k-elf.sh, and copy the results to artifacts/fuzix68k:
#   disk.img     the IDE disk (partition table, loader at LBA 1, kernel at
#                LBA 2, file system at LBA 2048; byte-swapped like the board)
#   fuzix.map    kernel link map (symbols for the ICE and the FUZIX monitor)
#   fuzix.elf    kernel with symbols
#   fuzix.sym    its symbols as nm output (for the ICE: --symbols)
#   offsets.txt  structure offsets for the FUZIX monitor (scripts/fuzix-offsets.c)
#   usersyms/    each program's symbols (NAME.sym, nm output), placed by the
#                ICE on the process running it (--symbols usersyms)
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
# Programs link through scripts/fuzix-ld68k-syms, which keeps their symbols.
# Without them yet, the programs already built are linked again (only
# linked: the objects stay).
export FUZIX_SYMS=$OUT/usersyms
LINK="LINKER=$ROOT/scripts/fuzix-ld68k-syms"
if [ ! -d "$FUZIX_SYMS" ]; then
	find "$FUZIX/Applications" -type f -size +32c | while read -r f; do
		if [ "$(head -c2 "$f" | od -An -tx1 | tr -d ' ')" = "03d0" ]; then rm -f "$f"; fi
	done
fi
app() { # dir [make args]
	d=$1
	shift
	(cd "$FUZIX/Applications/$d" &&
		FUZIX_ROOT=$FUZIX TARGET=tiny68k CPU=68000 USERCPU=68000 make -f Makefile.68000 "$LINK" "$@")
}
app as09 COPT=-fno-jump-tables
# MWC/cmd runs byacc for two grammars into the same y.tab.c, which a
# parallel make races on (one grammar's output ends up as the other's .c).
app MWC/cmd
(cd "$FUZIX" && make TARGET=tiny68k -j"$(nproc)" "$LINK" diskimage)
# A login on the second serial port too (DUART channel B, /dev/tty2; the
# emulator's --tty2): its getty is "off" in the stock inittab. ucp's "put"
# copies out of the image and "get" copies in.
IMG=$FUZIX/Images/tiny68k
TMP=$(mktemp -d)
UCP="$FUZIX/Standalone/ucp $IMG/filesys.img"
(cd "$TMP" && printf 'cd /etc\nput inittab\n' | $UCP >/dev/null)
if grep -q '^02:3:off:getty /dev/tty2' "$TMP/inittab"; then
	sed -i 's|^02:3:off:getty /dev/tty2|02:3:respawn:getty /dev/tty2|' "$TMP/inittab"
	(cd "$TMP" && printf 'cd /etc\nrm inittab\nget inittab\nchmod 0644 inittab\n' | $UCP >/dev/null)
	dd if="$IMG/filesys.img" of="$IMG/disk.img" bs=512 seek=2048 conv=notrunc,swab status=none
fi
# The terminal size for full-screen programs (less, vile): the tty has no
# size of its own. root's login shell runs ~/.profile.
(cd "$TMP" && printf 'cd /root\nput .profile\n' | $UCP >/dev/null 2>&1) || true
if ! grep -q 'stty rows 24 cols 80' "$TMP/.profile" 2>/dev/null; then
	echo 'stty rows 24 cols 80' >> "$TMP/.profile"
	(cd "$TMP" && printf 'cd /root\nrm .profile\nget .profile\nchmod 0644 .profile\n' | $UCP >/dev/null 2>&1)
	dd if="$IMG/filesys.img" of="$IMG/disk.img" bs=512 seek=2048 conv=notrunc,swab status=none
fi
rm -rf "$TMP"
cp "$IMG/disk.img" "$OUT/disk.img"
cp "$FUZIX/Kernel/fuzix.map" "$OUT/fuzix.map"
cp "$FUZIX/Kernel/platform/platform-tiny68k/fuzix.elf" "$OUT/fuzix.elf"
# Kernel symbols in nm form, which the ICE reads (--symbols).
m68k-elf-nm -n "$OUT/fuzix.elf" > "$OUT/fuzix.sym"
# Structure offsets the FUZIX monitor uses, from this kernel's headers.
K=$FUZIX/Kernel
m68k-elf-gcc -S -o - -m68000 -Os -fno-builtin -I"$K/cpu-68000" -I"$K/platform/platform-tiny68k" \
	-I"$K/include" "$ROOT/scripts/fuzix-offsets.c" | awk '$1 == ".equ" { sub(",", "", $2); print $2, $3 }' > "$OUT/offsets.txt"
echo "FUZIX $(git -C "$FUZIX" log -1 --format=%h) in ${OUT#$ROOT/}"
