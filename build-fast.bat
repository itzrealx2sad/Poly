@echo off
setlocal

:: ─────────────────────────────────────────────────
::  Poly Fast Build
::  Maximizes cargo build speed for iterative dev
:: ─────────────────────────────────────────────────

:: Parallelize codegen within each crate (8 logical cores)
set CARGO_BUILD_JOBS=8
set RUSTFLAGS=-C codegen-units=16 -C debuginfo=0

:: Share a single build cache across incremental rebuilds
set CARGO_INCREMENTAL=1

:: If you have sccache installed, enable it for cross-session caching
where sccache >nul 2>nul
if %ERRORLEVEL%==0 (
    echo [*] sccache detected, enabling shared compile cache
    set RUSTC_WRAPPER=sccache
)

echo.
echo ══════════════════════════════════════════
echo   Poly — Fast Dev Build
echo   Jobs: %CARGO_BUILD_JOBS%  Codegen units: 16
echo ══════════════════════════════════════════
echo.

:: Build only the final binary crate (pulls in workspace deps automatically)
:: Skips any crates not in the dependency chain
cargo build --package poly %*

echo.
if %ERRORLEVEL%==0 (
    echo [OK] Build succeeded
) else (
    echo [FAIL] Build failed with code %ERRORLEVEL%
)

endlocal
