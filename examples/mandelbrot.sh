#!/bin/sh
# Build the source disk, boot OS-9, compile C on the 6809, then render ASCII art.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
python3 scripts/build-c-disk.py
exec sh examples/nitros9-c.sh \
  --script examples/mandelbrot.tsv \
  --until MANDELBROT-DONE --max-cycles 1000000000 \
  --measure-from "Mandelbrot / BASIC09 to C / 6809" "$@"
