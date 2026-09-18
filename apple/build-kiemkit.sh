#!/bin/sh
# Regenerate apple/KiemKit (XCFramework + Swift package) from crates/kiem-ffi.
# Usage: apple/build-kiemkit.sh [--ios-simulator] [--release] [extra cargo-swift args]
# --ios-simulator builds only the host Mac's simulator architecture for fast local use.
# KiemKit is generated output — never edit it by hand.
set -eu

repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root/crates/kiem-ffi"

# Deliberate: the FFI package is built against macOS 14.0 even though the app
# project targets macOS 26.0. 14.0 is the intentional deployment-target floor
# for the KiemKit staticlib, independent of the app's own target/SDK, so this
# is NOT matched to the app's 26.0 — keep the two decoupled.
export MACOSX_DEPLOYMENT_TARGET=14.0
# Same for iOS: without this, cargo-swift links the cdylib at its 10.0 default
# while the object code is built for the installed SDK (26.x), failing with
# `___chkstk_darwin` / zstd "symbols not found" at link time.
export IPHONEOS_DEPLOYMENT_TARGET=26.0

# cargo-swift writes ./KiemKit relative to the crate; move it into apple/.
# Platforms: macOS (the Mac app + CLI), iOS device, and iOS simulator. macOS
# stays @14 (the linker floor the FFI has always used). iOS uses cargo-swift's
# default min (`.iOS(.v13)`) — NOT ios@26 — because cargo-swift pins the
# generated Package.swift to swift-tools-version:5.5, whose SupportedPlatform
# enum tops out well below .v26; a `.v26` token forces an impractical 6.2 bump
# that flips the package into Swift 6 mode and fails on UniFFI's non-Sendable
# `vtablePtr` statics. The app's own deployment target (iOS 26) still governs
# the build; this declared floor is only package-resumption boilerplate.
# cargo-swift itself passes the iOS SDK and target-linker flags the cdylib
# needs (plain `cargo build --target aarch64-apple-ios` can't link cdylib for
# iOS — those failing links are why we route through cargo-swift here).
if [ "${1:-}" = "--ios-simulator" ]; then
    shift
    case "$(uname -m)" in
    arm64) sim_target=aarch64-apple-ios-sim ;;
    x86_64) sim_target=x86_64-apple-ios ;;
    *)
        echo "Unsupported host architecture: $(uname -m)" >&2
        exit 1
        ;;
    esac
    set -- --target "$sim_target" "$@"
else
    set -- --platforms macos --platforms ios "$@"
fi

cargo swift package --name KiemKit --accept-all --silent "$@"
rm -rf "$repo_root/apple/KiemKit"
mv KiemKit "$repo_root/apple/KiemKit"
echo "KiemKit regenerated at apple/KiemKit"
