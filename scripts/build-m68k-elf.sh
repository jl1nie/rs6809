#!/bin/sh
# Build an m68k-elf cross toolchain (binutils + gcc, C only, with libgcc
# multilibs including -m68000) into artifacts/tools/m68k-elf, for FUZIX.
#
# FUZIX's 68000 build expects m68k-elf-gcc and links the 68000 libgcc
# (32-bit multiply/divide helpers); a Linux m68k compiler targets the 68020.
# The versions follow FUZIX's README.68000.md: gcc 10.3.0 configured
# --with-cpu=68000 is the author's known-good compiler (gcc 11 can fail to
# assemble, and newer defaults reject FUZIX's old-style C). Its warning:
# do not use an m68k-linux-gnu gcc. Sources are checked against SHA-256.
set -eu
cd "$(dirname "$0")/.."
ROOT=$PWD
TOOLS=$ROOT/artifacts/tools
SRC=$TOOLS/src
PREFIX=$TOOLS/m68k-elf
BUILD=$TOOLS/build-m68k-elf
JOBS=$(nproc)

BINUTILS=binutils-2.42
BINUTILS_SHA=f6e4d41fd5fc778b06b7891457b3620da5ecea1006c6a4a41ae998109f85a800
GCC=gcc-10.3.0
GCC_SHA=64f404c1a650f27fc33da242e1f2df54952e3963a49e06e73f6940f3223ac344
# A copy fetched earlier for gcc6809, if any.
GCC6809_EXTRA=$TOOLS/lwtools-4.25/extra

mkdir -p "$SRC" "$BUILD"

fetch() { # name url sha
	f=$SRC/$1
	if [ ! -f "$f" ]; then
		if [ -f "$GCC6809_EXTRA/$1" ]; then
			cp "$GCC6809_EXTRA/$1" "$f"
		else
			curl -fL -o "$f.part" "$2" && mv "$f.part" "$f"
		fi
	fi
	echo "$3  $f" | sha256sum -c -
}

fetch $BINUTILS.tar.xz https://ftp.gnu.org/gnu/binutils/$BINUTILS.tar.xz $BINUTILS_SHA
fetch $GCC.tar.xz https://ftp.gnu.org/gnu/gcc/$GCC/$GCC.tar.xz $GCC_SHA

[ -d "$SRC/$BINUTILS" ] || tar -C "$SRC" -xf "$SRC/$BINUTILS.tar.xz"
if [ ! -d "$SRC/$GCC" ]; then
	tar -C "$SRC" -xf "$SRC/$GCC.tar.xz"
	# GMP, MPFR, MPC and ISL in the tree (contrib/download_prerequisites
	# checks their sums).
	(cd "$SRC/$GCC" && ./contrib/download_prerequisites)
fi

if [ ! -x "$PREFIX/bin/m68k-elf-as" ]; then
	rm -rf "$BUILD/binutils" && mkdir -p "$BUILD/binutils"
	(cd "$BUILD/binutils" &&
		"$SRC/$BINUTILS/configure" --target=m68k-elf --prefix="$PREFIX" \
			--disable-nls --disable-werror --disable-gdb --disable-sim &&
		make -j"$JOBS" && make install)
fi

export PATH="$PREFIX/bin:$PATH"
if [ ! -x "$PREFIX/bin/m68k-elf-gcc" ]; then
	rm -rf "$BUILD/gcc" && mkdir -p "$BUILD/gcc"
	(cd "$BUILD/gcc" &&
		"$SRC/$GCC/configure" --target=m68k-elf --prefix="$PREFIX" \
			--enable-languages=c --without-headers --disable-nls \
			--disable-shared --disable-threads --disable-libssp \
			--disable-libquadmath --disable-libgomp --disable-libatomic \
			--disable-decimal-float --disable-libstdcxx --with-cpu=68000 \
			CFLAGS="-O2 -std=gnu11" CXXFLAGS="-O2 -std=gnu++11" &&
		make -j"$JOBS" all-gcc all-target-libgcc &&
		make install-gcc install-target-libgcc)
fi

"$PREFIX/bin/m68k-elf-gcc" --version | head -1
echo "68000 libgcc: $("$PREFIX/bin/m68k-elf-gcc" -m68000 -print-libgcc-file-name)"
