#!/bin/sh
# Collect the image files of an rs6809 release into artifacts/release and
# write their lines of release/images.sha256 (commit it: setup.sh checks
# downloads with it). The prebuilt emulators (rs6809-*) are built and
# uploaded by the release-binaries GitHub workflow; their lines are kept.
# Needs the builds: cargo build --release, scripts/fetch-cpm68k.py,
# scripts/build-fuzix-tiny68k.sh (and its --tn20k build, for the 8 MiB
# kernel of rs6809-fpga's Tang Nano 20K machine). Publishing (gh release create) is separate:
#   gh release create images-v1 artifacts/release/* --title ... --notes-file release/NOTICE.md
set -eu
cd "$(dirname "$0")/.."
OUT=artifacts/release
D=artifacts/cpm68k/dist/DISK8
F=artifacts/fuzix68k
for f in $D/CPM15000.SR $D/CPM15000.MAP $F/disk.img $F/fuzix.sym $F/usersyms/sh.sym; do
	[ -f "$f" ] || { echo "missing $f (see the comment at the top)" >&2; exit 1; }
done
rm -rf "$OUT"
mkdir -p "$OUT"
cp $D/CPM15000.SR $D/CPM15000.MAP "$OUT/"
python3 -I scripts/cpm68k-disk.py dist --no-alcyon "$OUT/cpm68k-a.img" examples/cpm68k/hello.c
gzip -n -9 "$OUT/cpm68k-a.img"
gzip -n -9 -c $F/disk.img > "$OUT/fuzix68k-disk.img.gz"
cp $F/fuzix.sym "$OUT/"
# The same for the 8 MiB kernel (rs6809-fpga), when it was built.
T=artifacts/fuzix68k-tn20k
if [ -f $T/disk.img ]; then
	gzip -n -9 -c $T/disk.img > "$OUT/fuzix68k-tn20k-disk.img.gz"
	cp $T/fuzix.sym "$OUT/fuzix-tn20k.sym"
	tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C $T -cf - usersyms | gzip -n -9 > "$OUT/fuzix-tn20k-usersyms.tar.gz"
fi
# Each program's symbols; the same bytes for the same files.
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C $F -cf - usersyms | gzip -n -9 > "$OUT/fuzix-usersyms.tar.gz"
cp release/NOTICE.md "$OUT/NOTICE.md"
{
	grep ' rs6809-' release/images.sha256 || true
	(cd "$OUT" && sha256sum -- *)
} | sort -k2 > release/images.sha256.new
mv release/images.sha256.new release/images.sha256
cat release/images.sha256
du -sh "$OUT"
