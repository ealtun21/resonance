# Build the real product installer in the e2e guest, as SYSTEM, for lifecycle.ps1.
# Mirrors .github/workflows/windows.yml: release binaries + the APO dll staged in target\release,
# then ISCC. Log: C:\lc-build.log (ends with "LC BUILD DONE"). Outputs, all in C:\lc:
#   setup-old.exe / setup-new.exe   same binaries, versions $ver.0 and $ver.1 (upgrade test)
#   resonance.exe, probe.ps1        the CLI and lifecycle-probe.ps1, independent of the install
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
# The tar-extracted sources carry the guest clock. If that clock was stepped back since the last
# build (w32time steps it by hours) cargo sees every workspace artifact as newer than its sources
# and silently ships stale binaries, so drop the workspace crates' fingerprints.
foreach ($t in 'release', 'apo') {
  Get-ChildItem "C:\src\target\$t\.fingerprint" -Directory -Filter 'resonance-*' -EA 0 | Remove-Item -Recurse -Force -EA 0
}
Step 'cargo' { cargo build --release -p resonance-daemon -p resonance-cli -p resonance-tui -p resonance-gui -p resonance-tray }
Step 'apo' { powershell -ExecutionPolicy Bypass -File C:\src\contrib\windows\build-apo.ps1 }
Step 'stage' { Copy-Item C:\src\target\apo\resonance_apo.dll C:\src\target\release\ -Force }
# resonance.ico is git-ignored (CI renders it from the GUI icon); a PNG-in-ICO of the tray icon will do.
Step 'icon' {
  $png = [IO.File]::ReadAllBytes('C:\src\crates\resonance-tray\assets\icon-64.png')
  $ms = New-Object IO.MemoryStream; $w = New-Object IO.BinaryWriter $ms
  $w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]1)                       # ICONDIR
  $w.Write([byte]64); $w.Write([byte]64); $w.Write([byte]0); $w.Write([byte]0)         # w h colors reserved
  $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$png.Length); $w.Write([uint32]22)
  $w.Write($png); [IO.File]::WriteAllBytes('C:\src\contrib\windows\resonance.ico', $ms.ToArray())
}
$iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
if (-not (Test-Path $iscc)) {
  Step 'innosetup-download' { curl.exe -sL -o C:\innosetup.exe https://github.com/jrsoftware/issrc/releases/download/is-6_7_3/innosetup-6.7.3.exe }
  Step 'innosetup-install' { Start-Process C:\innosetup.exe -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait }
}
$ver = if ($env:LC_VER) { $env:LC_VER } else { '0.0.0' }
New-Item -ItemType Directory -Force C:\lc | Out-Null
foreach ($n in 'old', 'new') {
  $v = "$ver." + $(if ($n -eq 'old') { 0 } else { 1 })
  Step "iscc-$n" { & $iscc "/DAppVersion=$v" C:\src\contrib\windows\resonance.iss }
  Copy-Item C:\src\contrib\windows\Output\resonance-setup.exe "C:\lc\setup-$n.exe" -Force
}
Copy-Item C:\src\target\release\resonance.exe C:\lc\resonance.exe -Force
Copy-Item C:\src\contrib\e2e\windows\lifecycle-probe.ps1 C:\lc\probe.ps1 -Force
"LC BUILD DONE" | Out-File $log -Append
