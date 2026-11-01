#!/bin/sh
# Boot the Level 2 multi-user disk prepared by scripts/demo-mandelbrot.py (Microware C in each home).
set -eu
cd "$(dirname "$0")/.."
# NitrOS-9 files from ./setup.sh, or a clone of the starter kit in reference/.
K=artifacts/nitros9
[ -f $K/6809M.bin ] || K=reference/nitros9-basic09-starter-kit/nitros9-runtime
exec sh scripts/rs6809 \
  --rom $K/6809M.bin \
  --disk artifacts/level2-mandel/multicomp09-level2.img \
  --ram-kb 512 --boot --telnet "$@"
