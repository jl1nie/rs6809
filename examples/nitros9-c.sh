#!/bin/sh
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
exec cargo run --release -- \
  --rom reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin \
  --disk artifacts/ccompiler/multicomp09-c.img \
  --boot "$@"
