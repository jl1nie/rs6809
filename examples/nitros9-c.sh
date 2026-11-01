#!/bin/sh
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
# NitrOS-9 files from ./setup.sh, or a clone of the starter kit in reference/.
K=artifacts/nitros9
[ -f $K/6809M.bin ] || K=reference/nitros9-basic09-starter-kit/nitros9-runtime
exec sh scripts/rs6809 \
  --rom $K/6809M.bin \
  --disk artifacts/ccompiler/multicomp09-c.img \
  --boot "$@"
