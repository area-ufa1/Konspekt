@echo off
rem Rust-тесты Konspekt в окружении MSVC (whisper.cpp собирается CMake + Ninja из VS).
for /f "usebackq delims=" %%i in (`"%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set VSROOT=%%i
call "%VSROOT%\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
set CMAKE_GENERATOR=Ninja
cd /d "%~dp0..\frontend\src-tauri" || exit /b 1
if not exist binaries mkdir binaries
if not exist binaries\llama-helper-x86_64-pc-windows-msvc.exe type nul > binaries\llama-helper-x86_64-pc-windows-msvc.exe
if not exist binaries\llama-helper-cpu-x86_64-pc-windows-msvc.exe type nul > binaries\llama-helper-cpu-x86_64-pc-windows-msvc.exe
if not exist runtime-deps mkdir runtime-deps
if not exist runtime-deps\placeholder.txt type nul > runtime-deps\placeholder.txt
if not defined CARGO_BUILD_JOBS set CARGO_BUILD_JOBS=4
cargo test %*
