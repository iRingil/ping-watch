@echo off
setlocal

:: Change directory to the project root.
:: %~dp0 is the script's directory, '../..' moves two levels up to the project root.
cd /d "%~dp0..\.."
echo Current working directory is: %cd%

:: The release build: target\release\ping-watch.exe.
cargo build --release --locked
if %errorlevel% neq 0 (
    echo ERROR: cargo build failed with error code %errorlevel%.
    exit /b %errorlevel%
)
set "BUILT_EXE=target\release\ping-watch.exe"

:: The license notices of the crates in the build (cargo install cargo-about --locked --features cli).
if not exist "build" mkdir "build"
cargo about generate about.hbs -o "build\THIRD-PARTY.html"
if %errorlevel% neq 0 (
    echo ERROR: cargo about failed with error code %errorlevel% - is cargo-about installed?
    exit /b %errorlevel%
)

:: Inno Setup 7 (Program Files\Inno Setup), or 6 for all users (CI runners) or the current user (winget --scope user).
set "ISCC_EXE=%ProgramFiles%\Inno Setup\ISCC.exe"
if not exist "%ISCC_EXE%" set "ISCC_EXE=%ProgramFiles(x86)%\Inno Setup 6\ISCC.exe"
if not exist "%ISCC_EXE%" set "ISCC_EXE=%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe"
if not exist "%ISCC_EXE%" (
    echo ERROR: Inno Setup not found, install it: winget install JRSoftware.InnoSetup
    exit /b 1
)

:: The version is read back from the executable's properties, the way the installer script does.
set "APP_VERSION="
for /f "usebackq delims=" %%v in (`powershell -NoProfile -Command "$v = (Get-Item '%BUILT_EXE%').VersionInfo; '{0}.{1}.{2}' -f $v.FileMajorPart, $v.FileMinorPart, $v.FileBuildPart"`) do set "APP_VERSION=%%v"
echo App version: %APP_VERSION%

:: Installer: build\PingWatch-<version>-setup.exe.
"%ISCC_EXE%" /Q "infrastructure\inno\PingWatch.iss"
if %errorlevel% neq 0 (
    echo.
    echo ERROR: Inno Setup failed with error code %errorlevel%.
    exit /b %errorlevel%
)

:: Portable zip: the program with the portable marker, under a folder of its own inside the archive.
set "PORTABLE_DIR=build\portable\PingWatch"
set "PORTABLE_ZIP=build\PingWatch-%APP_VERSION%-portable.zip"
if exist "build\portable" rmdir /s /q "build\portable"
mkdir "%PORTABLE_DIR%"
copy /y "%BUILT_EXE%" "%PORTABLE_DIR%\PingWatch.exe" >nul
copy /y "LICENSE.md" "%PORTABLE_DIR%\" >nul
copy /y "build\THIRD-PARTY.html" "%PORTABLE_DIR%\" >nul
copy /y "assets\fonts\OFL.txt" "%PORTABLE_DIR%\Roboto-Condensed-OFL.txt" >nul
copy /y "infrastructure\inno\portable.txt" "%PORTABLE_DIR%\" >nul
if exist "%PORTABLE_ZIP%" del "%PORTABLE_ZIP%"
:: Windows' own tar, not Git's GNU tar from PATH: zip paths with forward slashes (Compress-Archive writes backslashes).
"%SystemRoot%\System32\tar.exe" -a -c -f "%PORTABLE_ZIP%" -C "build\portable" PingWatch
if %errorlevel% neq 0 (
    echo ERROR: creating %PORTABLE_ZIP% failed.
    exit /b 1
)
rmdir /s /q "build\portable"

echo.
echo Script finished successfully: build\PingWatch-%APP_VERSION%-setup.exe and %PORTABLE_ZIP%.

endlocal
