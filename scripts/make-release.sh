#!/bin/sh
# Collect the files of an rs6809 image release into artifacts/release and
# write release/images.sha256 (commit it: setup.sh checks downloads with it).
# Needs the builds: cargo build --release, scripts/fetch-cpm68k.py,
# scripts/build-fuzix-tiny68k.sh. Publishing (gh release create) is separate:
#   gh release create images-v1 artifacts/release/* --title ... --notes-file release/NOTICE.md
set -eu
cd "$(dirname "$0")/.."
OUT=artifacts/release
D=artifacts/cpm68k/dist/DISK8
F=artifacts/fuzix68k
for f in $D/CPM15000.SR $D/CPM15000.MAP $F/disk.img $F/fuzix.sym; do
	[ -f "$f" ] || { echo "missing $f (see the comment at the top)" >&2; exit 1; }
done
rm -rf "$OUT"
mkdir -p "$OUT"
cargo build --release --quiet
cp target/release/rs6809 "$OUT/rs6809-linux-x86_64"
strip "$OUT/rs6809-linux-x86_64"
cp $D/CPM15000.SR $D/CPM15000.MAP "$OUT/"
python3 -I scripts/cpm68k-disk.py dist --no-alcyon "$OUT/cpm68k-a.img" examples/cpm68k/hello.c
gzip -n -9 "$OUT/cpm68k-a.img"
gzip -n -9 -c $F/disk.img > "$OUT/fuzix68k-disk.img.gz"
cp $F/fuzix.sym "$OUT/"
cp release/NOTICE.md "$OUT/NOTICE.md"
(cd "$OUT" && sha256sum -- * | sort -k2) > release/images.sha256
cat release/images.sha256
du -sh "$OUT"
