# Xcode Build Optimization Plan

## Project Context

- **Project:** `apple/Kiem.xcodeproj`
- **Scheme:** `Kiem iOS`
- **Configuration:** `Debug`
- **Destination:** `platform=iOS Simulator,id=90085449-E732-4F61-A7DB-FA3B1302F027`
- **Xcode:** Xcode 26.6 Build version 17F113
- **macOS:** macOS-26.5.2-arm64-arm-64bit-Mach-O
- **Date:** 2026-09-11T09:51:01.717756+00:00
- **Benchmark artifact:** `.build-benchmark/20260911T095025Z-kiem-ios.json`

## Baseline Benchmarks

| Metric | Clean | Zero-Change |
|--------|-------|-------------|
| Median | 4.891s | 0.786s |
| Min | 4.703s | 0.777s |
| Max | 4.908s | 0.799s |
| Runs | 3 | 3 |

Both benchmark groups have low variance (under 5% of their medians). Zero-change measures fixed overhead, not a touched-file incremental rebuild.

## Implemented Results

All three recommendations were applied on 2026-09-18.

| Metric | Before | After | Change |
|--------|-------:|------:|-------:|
| Clean median | 4.891s | 3.359s | 1.532s faster (31%) |
| Zero-change median | 0.786s | 0.821s | 0.035s slower (noise-level) |
| Simulator-only KiemKit, warm | — | 2.09s | 623 MB output |

The simulator-only mode is opt-in (`apple/build-kiemkit.sh --ios-simulator`); the default remains universal. The previous universal XCFramework was 3.1 GB.

A corrected compilation-cache check that preserved `CompilationCache.noindex` measured a 3.824s median with 163/163 cache hits. The benchmark helper's reported 8.817s cached-clean median is invalid because it deleted the cache together with DerivedData. An eager-linking A/B with a warm cache measured 3.149s enabled versus 3.330s disabled.

Post-change artifact: `.build-benchmark/20260918T082930Z-kiem-ios.json`.

### Clean Build Timing Summary

> **Note:** These are aggregated task times across all CPU cores. Because Xcode runs many tasks in parallel, these totals typically exceed the actual build wait time shown above. A large number here does not mean it is blocking your build.

| Category | Tasks | Seconds |
|----------|------:|--------:|
| SwiftCompile | 21 | 9.470s |
| Ld | 5 | 1.091s |
| CompileAssetCatalogVariant | 1 | 0.932s |
| SwiftEmitModule | 3 | 0.922s |
| SwiftDriver | 3 | 0.486s |
| CodeSign | 3 | 0.282s |
| PhaseScriptExecution | 1 | 0.184s |
| ExtractAppIntentsMetadata | 3 | 0.050s |
| ConstructStubExecutorLinkFileList | 1 | 0.041s |
| CopySwiftLibs | 1 | 0.034s |
| Copy | 14 | 0.025s |
| RegisterExecutionPolicyException | 3 | 0.015s |
| AppIntentsSSUTraining | 1 | 0.014s |
| GenerateAssetSymbols | 1 | 0.011s |
| WriteAuxiliaryFile | 33 | 0.007s |
| ProcessProductPackagingDER | 1 | 0.007s |
| ProcessInfoPlistFile | 1 | 0.006s |
| SwiftDriver Compilation Requirements | 3 | 0.002s |
| Touch | 1 | 0.002s |
| LinkAssetCatalog | 1 | 0.002s |
| SwiftDriver Compilation | 3 | 0.001s |
| SwiftMergeGeneratedHeaders | 3 | 0.001s |
| ProcessProductPackaging | 1 | 0.001s |
| Validate | 1 | 0.000s |

### Zero-Change Build Timing Summary

> **Note:** These are aggregated task times across all CPU cores. Because Xcode runs many tasks in parallel, these totals typically exceed the actual build wait time shown above. A large number here does not mean it is blocking your build.

| Category | Tasks | Seconds |
|----------|------:|--------:|
| PhaseScriptExecution | 1 | 0.011s |

## Build Settings Audit

### Debug Configuration

- [x] `SWIFT_COMPILATION_MODE`: `(unset)` (recommended: `incremental`)
- [x] `SWIFT_OPTIMIZATION_LEVEL`: `-Onone` (recommended: `-Onone`)
- [x] `GCC_OPTIMIZATION_LEVEL`: `0` (recommended: `0`)
- [x] `ONLY_ACTIVE_ARCH`: `YES` (recommended: `YES`)
- [x] `DEBUG_INFORMATION_FORMAT`: `dwarf` (recommended: `dwarf`)
- [x] `ENABLE_TESTABILITY`: `YES` (recommended: `YES`)
- [x] `EAGER_LINKING`: `YES`

### General (All Configurations)

- [x] `COMPILATION_CACHE_ENABLE_CACHING`: `YES`
- [x] `SWIFT_USE_INTEGRATED_DRIVER`: inherited modern Xcode default
- [x] `CLANG_ENABLE_MODULES`: `YES`
- [x] `SWIFT_ENABLE_EXPLICIT_MODULES`: `YES`

### Release Configuration

- [x] `SWIFT_COMPILATION_MODE`: `wholemodule` (recommended: `wholemodule`)
- [x] `SWIFT_OPTIMIZATION_LEVEL`: `-O` (recommended: `-O`)
- [x] `GCC_OPTIMIZATION_LEVEL`: not applicable; no first-party C/ObjC sources
- [x] `ONLY_ACTIVE_ARCH`: `NO` (recommended: `NO`)
- [x] `DEBUG_INFORMATION_FORMAT`: `dwarf-with-dsym` (recommended: `dwarf-with-dsym`)
- [x] `ENABLE_TESTABILITY`: `NO` (recommended: `NO`)

### Cross-Target Consistency

- [x] `SWIFT_COMPILATION_MODE` is consistent across all targets
- [x] `SWIFT_OPTIMIZATION_LEVEL` is consistent across all targets
- [x] `ONLY_ACTIVE_ARCH` is consistent across all targets
- [x] `DEBUG_INFORMATION_FORMAT` is consistent across all targets

## Compilation Diagnostics

Threshold: 100ms | Total warnings: 0 | Function bodies: 0 | Expressions: 0

No type-checking hotspots found above threshold. Xcode 26.6 rejected the skill's optional `-debug-time-compilation` flag, so the successful diagnostics run used the supported function/expression checks only.

## Root Cause

The Xcode build is not slow: a clean simulator build is 4.891s and a zero-change rebuild is 0.786s. The long wait came from the prerequisite Rust packaging step, `apple/build-kiemkit.sh`, which silently compiles a 478-dependency graph for five Apple architecture targets and emits a 3.1 GB universal XCFramework. That work is necessary for release distribution but excessive for a local arm64 iOS Simulator build.

The freshness script is not a meaningful bottleneck: it costs 11ms on a zero-change build. Pulp has one 26-file production target, no remote dependencies, no plugins, no macros, and no graph issue worth restructuring.

## Prioritized Recommendations

### 1. Add a simulator-only KiemKit development build

**Wait-Time Impact:** Expected to cut first-time local KiemKit regeneration substantially; the exact wall-clock reduction is uncertain until benchmarked, but it avoids four of the five architecture builds used by the current universal package.
**Actionability:** repo-local
**Category:** build-script
**Evidence:**

- apple/build-kiemkit.sh always packages macOS and iOS universally, even when the developer only needs an arm64 iOS Simulator slice.
- The current output contains macOS arm64+x86_64, iOS arm64, and iOS Simulator arm64+x86_64 slices and is 3.1 GB.
- Cargo has separate output trees for five Apple target triples; kiem-ffi's dependency graph contains 478 unique normal dependencies.
- cargo-swift 0.11.1 supports --target aarch64-apple-ios-sim, while release scripts can keep using the full universal build.
**Impact:** High for fresh/stale KiemKit generation; no effect on ordinary Xcode builds once KiemKit exists
**Confidence:** High that less work is performed; wall-clock delta needs a controlled benchmark
**Risk:** Low if opt-in and release/archive paths remain universal
**Scope:** apple/build-kiemkit.sh local-development path only

### 2. Enable Xcode compilation caching

**Wait-Time Impact:** Measured by the skill at 5-14% faster clean builds across tested projects; on this 4.891s clean baseline that is roughly 0.2-0.7s, with the benefit compounding during branch switches and Clean Build Folder.
**Actionability:** repo-local
**Category:** build-settings
**Evidence:**

- Xcode 26.6 resolves no COMPILATION_CACHE_ENABLE_CACHING value for the Kiem iOS target, so Swift/Clang compilation caching is not enabled.
- SwiftCompile is the largest clean-build category, although its 9.47s average task time is parallelized inside a 4.891s wall-clock build.
- The installed skill calls this COMPILATION_CACHING; Xcode 26.6's actual umbrella build-setting key is COMPILATION_CACHE_ENABLE_CACHING.
**Impact:** Low absolute clean-build improvement because the project is already small and fast
**Confidence:** Medium
**Risk:** Low
**Scope:** Shared XcodeGen build settings

### 3. Enable eager linking for Debug

**Wait-Time Impact:** Impact on wait time is uncertain -- likely below 0.5s on the current 4.891s clean build; re-benchmark after applying to confirm.
**Actionability:** repo-local
**Category:** build-settings
**Evidence:**

- EAGER_LINKING resolves to NO for Debug.
- Linking accounts for about 1.1s of aggregate clean-build task time, but much of the build is already parallelized.
**Impact:** Low
**Confidence:** Low to medium
**Risk:** Low
**Scope:** Debug configuration

## Approval Checklist

- [x] **1. Add a simulator-only KiemKit development build**
- [x] **2. Enable Xcode compilation caching**
- [x] **3. Enable eager linking for Debug**

## Verification

- `sh -n apple/build-kiemkit.sh`
- `cd apple && xcodegen generate`
- `xcodebuild` succeeds for the `Kiem iOS` Debug scheme on iPhone 17 Pro Simulator
- Resolved settings contain `COMPILATION_CACHE_ENABLE_CACHING = YES` and `EAGER_LINKING = YES`
