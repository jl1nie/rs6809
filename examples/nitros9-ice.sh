#!/bin/sh
# NitrOS-9 Level 1 under the ICE. Open the printed URL in a browser.
set -eu
cd "$(dirname "$0")/.."
# NitrOS-9 files from ./setup.sh, or a clone of the starter kit in reference/.
K=artifacts/nitros9
[ -f $K/6809M.bin ] || K=reference/nitros9-basic09-starter-kit/nitros9-runtime
exec sh scripts/rs6809 \
  --rom $K/6809M.bin \
  --disk $K/multicomp09_sd.img \
  --boot --ice "$@"
