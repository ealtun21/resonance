# Build the real product installer (resonance-setup.exe) in the e2e guest, as SYSTEM.
# Mirrors .github/workflows/windows.yml: release binaries + APO dll staged in target\release,
# then ISCC. Needs C:\src\contrib\windows\resonance.ico (the host generates a placeholder).
# Log: C:\lc-build.log, ends with "LC BUILD DONE"; the installer lands in
# C:\src\contrib\windows\Output\resonance-setup.exe. $env:LC_VER overrides the version
# (used to build a second, "newer" installer for the upgrade-over-running-install test).
$ErrorActionPreference = 'Continue'
$env:Path = "C:\Windows\System32\config\systemprofile\.cargo\bin;C:\Users\Quickemu\.cargo\bin;" + $env:Path
$env:CARGO_TERM_COLOR = 'never'; $env:CARGO_INCREMENTAL = '0'
Set-Location C:\src
$log = 'C:\lc-build.log'
"start" | Out-File $log
function Step($name, [scriptblock]$body) {
  "== $name" | Out-File $log -Append
  & $body *>> $log
  "== $name exit $LASTEXITCODE" | Out-File $log -Append
}
Step 'cargo' { cargo build --release -p resonance-daemon -p resonance-cli -p resonance-tui -p resonance-gui -p resonance-tray }
Step 'apo' { powershell -ExecutionPolicy Bypass -File C:\src\contrib\windows\build-apo.ps1 }
Step 'stage' { Copy-Item C:\src\target\apo\resonance_apo.dll C:\src\target\release\ -Force }
$iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
if (-not (Test-Path $iscc)) {
  Step 'innosetup-download' { curl.exe -sL -o C:\innosetup.exe https://github.com/jrsoftware/issrc/releases/download/is-6_7_3/innosetup-6.7.3.exe }
  Step 'innosetup-install' { Start-Process C:\innosetup.exe -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART' -Wait }
}
$ver = if ($env:LC_VER) { $env:LC_VER } else { '0.11.1' }
Step 'iscc' { & $iscc "/DAppVersion=$ver" C:\src\contrib\windows\resonance.iss }
"LC BUILD DONE" | Out-File $log -Append
