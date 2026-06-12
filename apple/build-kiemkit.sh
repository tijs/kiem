#!/bin/sh
# Regenerate apple/KiemKit (XCFramework + Swift package) from crates/kiem-ffi.
# Usage: apple/build-kiemkit.sh [--release] [extra cargo-swift args]
# KiemKit is generated output — never edit it by hand.
set -eu

repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root/crates/kiem-ffi"

# Match the app's deployment target so the linker doesn't warn about every
# object file in the staticlib (project.yml sets 14.0 too).
export MACOSX_DEPLOYMENT_TARGET=14.0

# cargo-swift writes ./KiemKit relative to the crate; move it into apple/.
cargo swift package --platforms macos --name KiemKit --accept-all --silent "$@"
rm -rf "$repo_root/apple/KiemKit"
mv KiemKit "$repo_root/apple/KiemKit"
echo "KiemKit regenerated at apple/KiemKit"
