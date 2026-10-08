# Build the daemon, the e2e agent and the APO in the VM, then install the APO with the
# repo's own installer (run as SYSTEM: rustup lives in its profile). Log: C:\build.log.
$ErrorActionPreference = 'Continue'
$env:Path = "C:\Windows\System32\config\systemprofile\.cargo\bin;C:\Users\Quickemu\.cargo\bin;" + $env:Path
$env:CARGO_TERM_COLOR = 'never'
$env:CARGO_INCREMENTAL = '0'   # incremental + tar-stamped mtimes gave bogus link errors
Set-Location C:\src
$log = 'C:\build.log'
function Step($name, [scriptblock]$body) {
  "== $name" | Out-File $log -Append
  & $body *>> $log
  "== $name exit $LASTEXITCODE" | Out-File $log -Append
}
"start" | Out-File $log
Step 'agent' { cargo build --profile e2e-build -p resonance-daemon -p resonance-e2e }
Step 'apo' { powershell -ExecutionPolicy Bypass -File C:\src\contrib\windows\build-apo.ps1 }
$dll = Get-ChildItem C:\src\target -Recurse -Filter resonance_apo.dll -EA 0 | sort LastWriteTime | select -Last 1
"apo dll: $($dll.FullName)" | Out-File $log -Append
Step 'install' {
  Stop-Service Audiosrv -Force
  New-Item -ItemType Directory -Force 'C:\Program Files\Resonance' | Out-Null
  Copy-Item $dll.FullName 'C:\Program Files\Resonance\resonance_apo.dll' -Force
  powershell -ExecutionPolicy Bypass -File C:\src\contrib\windows\install-apo.ps1 -DllPath 'C:\Program Files\Resonance\resonance_apo.dll'
  # Scream is a legacy WDM device: only the GFX slot is honoured (spec section 14, W0).
  powershell -ExecutionPolicy Bypass -File C:\src\contrib\e2e\windows\attach-slot.ps1 -Slot 2 -Name Scream
}
"BUILD DONE" | Out-File $log -Append
