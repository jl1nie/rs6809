#!/bin/sh
# NitrOS-9 Level 1 under the ICE. Open the printed URL in a browser.
set -eu
cd "$(dirname "$0")/.."
exec cargo run --release -- \
  --rom reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin \
  --disk reference/nitros9-basic09-starter-kit/nitros9-runtime/multicomp09_sd.img \
  --boot --ice "$@"
