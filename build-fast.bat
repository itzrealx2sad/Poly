@echo off
setlocal

set "CARGO_BUILD_JOBS=%NUMBER_OF_PROCESSORS%"
if not defined CARGO_BUILD_JOBS set "CARGO_BUILD_JOBS=4"
set "CARGO_INCREMENTAL=1"
set "CARGO_PROFILE_DEV_DEBUG=0"
set "CARGO_PROFILE_DEV_INCREMENTAL=true"
set "CARGO_PROFILE_DEV_CODEGEN_UNITS=16"

where sccache >nul 2>nul
if not errorlevel 1 (
    echo [*] sccache detected, enabling shared compile cache
    set "RUSTC_WRAPPER=sccache"
)

echo.
echo ==========================================
echo   Poly - Fast Development Build
echo   Jobs: %CARGO_BUILD_JOBS%  Codegen units: 16
echo ==========================================
echo.

cargo build --locked --package poly %*
set "BUILD_EXIT=%ERRORLEVEL%"

echo.
if "%BUILD_EXIT%"=="0" (
    echo [OK] Build succeeded
) else (
    echo [FAIL] Build failed with code %BUILD_EXIT%
)

endlocal & exit /b %BUILD_EXIT%
