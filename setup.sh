#!/bin/sh
# One-shot setup: everything needed to run NitrOS-9 (6809), CP/M-68K and
# FUZIX (68000) in rs6809. Then: ./run.sh nitros9 | cpm68k | fuzix [--ice]
#
#   1. the emulator: cargo build when Rust 1.89 or newer is installed,
#      otherwise the prebuilt binary from the GitHub release (Linux x86-64,
#      statically linked; macOS on Apple Silicon). Nothing is installed on
#      the system: everything stays in this directory.
#   2. NitrOS-9 Level 1 (Multicomp boot ROM and SD image) from the
#      nitros9-basic09-starter-kit at a fixed commit (not redistributed)
#   3. CP/M-68K and FUZIX images from the GitHub release
#   4. the Alcyon C compiler suite onto the CP/M-68K disk, from the
#      original CP/M-68K 1.3 distribution (not redistributed)
#
# Every download is checked against the SHA-256 kept in this repository
# (release/images.sha256 and below). Running it again only fetches what is
# missing. RS6809_RELEASE_URL points at another copy of the release files.
set -eu
cd "$(dirname "$0")"

TAG=images-v1
RELEASE_URL=${RS6809_RELEASE_URL:-https://github.com/jl1nie/rs6809/releases/download/$TAG}
KIT=https://raw.githubusercontent.com/kyo-ta04/nitros9-basic09-starter-kit/bf60370cfa180636cde3ee305d089d94872de82a/nitros9-runtime
ROM_SHA=510dea6bc49787728ba33c414b38178fa89ca2bad47a47164f30af2fe7c3d2b5
SD_SHA=6707a339f13d94a5eab7a4bc6c80d1f4bf0ac9b6185218fd9ecdd8c7aa413ff5

say() { printf '\n== %s\n' "$*"; }
die() { printf 'setup: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required${2:+ ($2)}"; }

if command -v sha256sum >/dev/null 2>&1; then
	sha() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
	sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
	die "sha256sum or shasum is required"
fi
need curl
need gzip

# fetch URL FILE SHA: download unless FILE already has SHA.
fetch() {
	if [ -f "$2" ] && [ "$(sha "$2")" = "$3" ]; then
		return
	fi
	mkdir -p "$(dirname "$2")"
	echo "  $1"
	curl -fsSL --retry 3 -o "$2.part" "$1" || die "download failed: $1"
	got=$(sha "$2.part")
	[ "$got" = "$3" ] || { rm -f "$2.part"; die "SHA-256 mismatch for $1 ($got)"; }
	mv "$2.part" "$2"
}
# The SHA-256 of a release file, from release/images.sha256.
rsha() { awk -v f="$1" '$2 == f { print $1 }' release/images.sha256; }
# release FILE DEST: fetch a release file into DEST.
release() { fetch "$RELEASE_URL/$1" "$2" "$(rsha "$1")"; }

say "1/4 emulator"
MSRV=1.89
# rustc_ok: rustc is at least MSRV (the 2024 edition).
rustc_ok() {
	v=$(rustc --version 2>/dev/null | cut -d' ' -f2) || return 1
	[ -n "$v" ] || return 1
	[ "$(printf '%s\n%s\n' "$MSRV" "$v" | sort -t. -k1,1n -k2,2n -k3,3n | head -1)" = "$MSRV" ]
}
prebuilt() {
	case "$(uname -s)-$(uname -m)" in
	Linux-x86_64) asset=rs6809-linux-x86_64 ;;
	Darwin-arm64) asset=rs6809-macos-arm64 ;;
	*) die "no prebuilt emulator for $(uname -s)-$(uname -m): install Rust $MSRV or newer (https://rustup.rs) and run again" ;;
	esac
	release "$asset" bin/rs6809
	chmod +x bin/rs6809
	echo "  using the prebuilt $asset (bin/rs6809)"
}
if command -v cargo >/dev/null 2>&1 && rustc_ok; then
	if cargo build --release; then
		rm -f bin/rs6809
	else
		echo "  cargo build failed: falling back to the prebuilt emulator"
		prebuilt
	fi
else
	if command -v cargo >/dev/null 2>&1; then
		echo "  Rust $(rustc --version 2>/dev/null | cut -d' ' -f2) is older than $MSRV (rustup update upgrades it)"
	fi
	prebuilt
fi

say "2/4 NitrOS-9 (from the nitros9-basic09-starter-kit)"
fetch "$KIT/6809M.bin" artifacts/nitros9/6809M.bin "$ROM_SHA"
fetch "$KIT/multicomp09_sd.img" artifacts/nitros9/multicomp09_sd.img "$SD_SHA"

say "3/4 CP/M-68K and FUZIX (from the release)"
release CPM15000.SR artifacts/cpm68k/dist/DISK8/CPM15000.SR
release CPM15000.MAP artifacts/cpm68k/dist/DISK8/CPM15000.MAP
release cpm68k-a.img.gz artifacts/release-cache/cpm68k-a.img.gz
release fuzix68k-disk.img.gz artifacts/release-cache/fuzix68k-disk.img.gz
release fuzix.sym artifacts/fuzix68k/fuzix.sym
release fuzix-usersyms.tar.gz artifacts/release-cache/fuzix-usersyms.tar.gz
release NOTICE.md artifacts/release-cache/NOTICE.md
[ -f artifacts/fuzix68k/disk.img ] || gzip -dc artifacts/release-cache/fuzix68k-disk.img.gz > artifacts/fuzix68k/disk.img
# The programs' symbols for the ICE (artifacts/fuzix68k/usersyms/NAME.sym).
[ -f artifacts/fuzix68k/usersyms/sh.sym ] || gzip -dc artifacts/release-cache/fuzix-usersyms.tar.gz | tar -xf - -C artifacts/fuzix68k
fresh_cpm=
if [ ! -f artifacts/cpm68k/a.img ]; then
	gzip -dc artifacts/release-cache/cpm68k-a.img.gz > artifacts/cpm68k/a.img
	fresh_cpm=1
fi

say "4/4 Alcyon C for CP/M-68K (from the original distribution)"
if [ -n "$fresh_cpm" ]; then
	if command -v python3 >/dev/null 2>&1 && command -v unzip >/dev/null 2>&1 &&
		python3 -I scripts/fetch-cpm68k.py; then
		python3 -I scripts/cpm68k-disk.py alcyon artifacts/cpm68k/a.img
		# The assembler's symbol table (AS68SYMB.DAT) is made once, on CP/M.
		printf 'A>\tAS68 -I AS68INIT\n' > artifacts/cpm68k/as68init.script
		sh scripts/rs6809 --cpu 68000 --machine tiny68k \
			--bin artifacts/cpm68k/dist/DISK8/CPM15000.SR --bin examples/cpm68k/bios.sr --pc 0x15000 \
			--disk artifacts/cpm68k/a.img --persist-disk --script artifacts/cpm68k/as68init.script \
			--until "assembler initialized" --max-cycles 2000000000 </dev/null >/dev/null ||
			die "AS68 -I AS68INIT failed on CP/M-68K"
		rm -f artifacts/cpm68k/as68init.script
		echo "  added to artifacts/cpm68k/a.img (and AS68 initialized)"
	else
		echo "  skipped (needs python3, unzip and http://www.cpm.z80.de): CP/M-68K runs without the C compiler"
	fi
else
	echo "  artifacts/cpm68k/a.img already set up (delete it and run again to start over)"
fi

cat <<'EOF'

Done. Run one of:
  ./run.sh nitros9          NitrOS-9 Level 1 (6809, Multicomp)
  ./run.sh cpm68k           CP/M-68K 1.3 (68000, Tiny68K)
  ./run.sh fuzix            FUZIX 0.5 (68000, Tiny68K)
Add --ice for the debugger at http://127.0.0.1:6809/ (--dap for VS Code).
Licences of the images: artifacts/release-cache/NOTICE.md
EOF
