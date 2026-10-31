#!/bin/sh
# Boot the Level 2 multi-user disk prepared by scripts/demo-mandelbrot.py (Microware C in each home).
set -eu
cd "$(dirname "$0")/.."
exec cargo run --release -- \
  --rom reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin \
  --disk artifacts/level2-mandel/multicomp09-level2.img \
  --ram-kb 512 --boot --telnet "$@"
