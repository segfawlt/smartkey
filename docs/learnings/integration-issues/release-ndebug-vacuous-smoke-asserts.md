---
title: Release `-DNDEBUG` Silently Disabled `addon_load_smoke` Assertions
category: integration-issues
module: adapters/fcitx5-smartkey/tests/addon_load_smoke.cpp
tags: [fcitx5, cmake, testing, ndebug]
problem_type: integration-issue
date: 2026-11-18
---

# Release `-DNDEBUG` Silently Disabled `addon_load_smoke` Assertions

## Problem

`install-local.sh` configures CMake with `CMAKE_BUILD_TYPE=Release`, whose
`CMAKE_CXX_FLAGS_RELEASE` is `-O3 -DNDEBUG`. `addon_load_smoke.cpp` checks everything with
`<cassert>` `assert`, so every check compiled to a no-op and `ctest` always printed PASSED.

## Symptoms

- A stale expectation (`formatSessionCompact(SmartKeySessionSnapshot{}) == "Idle"`; a zeroed
  snapshot actually formats as `Unknown[lc=0]`) had been failing unnoticed.
- New red/green tests appear green before the fix exists.

## Solution

`#undef NDEBUG` before `#include <cassert>` at the top of `addon_load_smoke.cpp`, then fix the
expectations it exposed.

## Prevention

- When adding a red test, confirm it actually fails before implementing the fix — a test that is
  green before the fix is the signal that assertions are compiled out.
- Any new C++ test target built under Release must either `#undef NDEBUG` or use a
  non-`assert` check macro.

## References

1
