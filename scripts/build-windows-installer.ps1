$ErrorActionPreference = "Stop"

Write-Host "Building release binaries..."
cargo build --release -p kvm-server -p kvm-client

Write-Host "Creating installer directory structure..."
$installerDir = "installer-windows"
if (Test-Path $installerDir) {
    Remove-Item -Recurse -Force $installerDir
}
New-Item -ItemType Directory -Path $installerDir | Out-Null

Copy-Item -Path "target\release\kvm-server.exe" -Destination "$installerDir\"
Copy-Item -Path "target\release\kvm-client.exe" -Destination "$installerDir\"

if (Test-Path "README.md") {
    Copy-Item -Path "README.md" -Destination "$installerDir\"
}

# NOTE: INSTALL_DIR is set WITHOUT quotes here and quoted individually at each
# use site below. Setting it with embedded quotes (the previous bug) breaks
# any context where the value needs to be concatenated with another literal
# (e.g. the VBScript TargetPath line), because the quote ends up in the middle
# of the string instead of around the whole thing.
$installBat = @"
@echo off
setlocal EnableDelayedExpansion
echo Installing KVM-RS...

net session >nul 2>&1
if not "%errorlevel%"=="0" (
    echo This installer must be run as Administrator.
    pause
    exit /b 1
)

set INSTALL_DIR=%ProgramFiles%\KVM-RS
if not exist "%INSTALL_DIR%" mkdir "%INSTALL_DIR%"

echo Copying binaries...
copy /Y kvm-server.exe "%INSTALL_DIR%\"
copy /Y kvm-client.exe "%INSTALL_DIR%\"
if exist README.md copy /Y README.md "%INSTALL_DIR%\"

echo Adding firewall rules...
netsh advfirewall firewall add rule name="KVM-RS Control TCP" dir=in action=allow protocol=TCP localport=4000
netsh advfirewall firewall add rule name="KVM-RS File Transfer TCP" dir=in action=allow protocol=TCP localport=4001
rem If you enabled the optional SOCKS5 relay (--socks5-port), open its port too:
rem netsh advfirewall firewall add rule name="KVM-RS SOCKS5" dir=in action=allow protocol=TCP localport=1080

echo Creating Start Menu shortcuts...
set SHORTCUT_SCRIPT=%TEMP%\create_shortcut.vbs
echo Set oWS = WScript.CreateObject("WScript.Shell") > "%SHORTCUT_SCRIPT%"
echo sLinkFile = "%ProgramData%\Microsoft\Windows\Start Menu\Programs\KVM-RS Server.lnk" >> "%SHORTCUT_SCRIPT%"
echo Set oLink = oWS.CreateShortcut(sLinkFile) >> "%SHORTCUT_SCRIPT%"
echo oLink.TargetPath = "%INSTALL_DIR%\kvm-server.exe" >> "%SHORTCUT_SCRIPT%"
echo oLink.Save >> "%SHORTCUT_SCRIPT%"
cscript /nologo "%SHORTCUT_SCRIPT%"
del "%SHORTCUT_SCRIPT%"

echo Adding install directory to the machine PATH...
rem NOTE: we deliberately do NOT use "setx /M PATH ..." here. setx truncates
rem its value argument at 1024 characters with no warning or error, and a
rem real machine's System PATH is very commonly already close to or over
rem that limit -- using setx here can silently corrupt the machine-wide
rem PATH and break unrelated software. Instead we write the registry value
rem directly with "reg add", which has no such length cap, and we verify the
rem write actually stuck before reporting success.
for /f "usebackq tokens=2,*" %%A in (`reg query "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment" /v Path`) do set "CURRENT_PATH=%%B"
echo "!CURRENT_PATH!" | find /I "%INSTALL_DIR%" >nul
if errorlevel 1 (
    reg add "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment" /v Path /t REG_EXPAND_SZ /d "!CURRENT_PATH!;%INSTALL_DIR%" /f >nul
    if errorlevel 1 (
        echo Failed to update the machine PATH. Add "%INSTALL_DIR%" to it manually if you want kvm-server/kvm-client on PATH.
    ) else (
        echo Added "%INSTALL_DIR%" to the machine PATH. Open a new terminal for it to take effect.
    )
) else (
    echo Install directory is already on the machine PATH.
)

echo Installation complete!
pause
"@

$installBat | Out-File -FilePath "$installerDir\install.bat" -Encoding ASCII

$uninstallBat = @"
@echo off
setlocal EnableDelayedExpansion
echo Uninstalling KVM-RS...

net session >nul 2>&1
if not "%errorlevel%"=="0" (
    echo This uninstaller must be run as Administrator.
    pause
    exit /b 1
)

set INSTALL_DIR=%ProgramFiles%\KVM-RS

echo Removing firewall rules...
netsh advfirewall firewall delete rule name="KVM-RS Control TCP"
netsh advfirewall firewall delete rule name="KVM-RS File Transfer TCP"
rem install.bat only creates the SOCKS5 rule if you uncommented it there, so this
rem rule normally does not exist; suppress netsh's harmless "No rules match the
rem specified criteria" message so the common case doesn't look like an error.
netsh advfirewall firewall delete rule name="KVM-RS SOCKS5" >nul 2>&1

echo Removing shortcuts...
del "%ProgramData%\Microsoft\Windows\Start Menu\Programs\KVM-RS Server.lnk" 2>nul

echo Removing install directory from the machine PATH...
rem See the matching note in install.bat: we use "reg add" instead of
rem "setx /M PATH ..." because setx silently truncates values over 1024
rem characters, which could corrupt the machine PATH on a real machine.
for /f "usebackq tokens=2,*" %%A in (`reg query "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment" /v Path`) do set "CURRENT_PATH=%%B"
set "NEWPATH=!CURRENT_PATH:;%INSTALL_DIR%=!"
set "NEWPATH=!NEWPATH:%INSTALL_DIR%;=!"
if not "!NEWPATH!"=="!CURRENT_PATH!" (
    reg add "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment" /v Path /t REG_EXPAND_SZ /d "!NEWPATH!" /f >nul
    if errorlevel 1 (
        echo Failed to update the machine PATH. Remove "%INSTALL_DIR%" from it manually if needed.
    ) else (
        echo Removed "%INSTALL_DIR%" from the machine PATH.
    )
)

echo Removing binaries...
del /F /Q "%INSTALL_DIR%\*.*" 2>nul
rmdir "%INSTALL_DIR%" 2>nul

echo Uninstallation complete!
pause
"@

$uninstallBat | Out-File -FilePath "$installerDir\uninstall.bat" -Encoding ASCII

Write-Host "Done! The installer package is in the '$installerDir' directory."
