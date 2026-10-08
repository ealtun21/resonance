$ErrorActionPreference = 'Continue'
$env:Path = "C:\Users\Quickemu\.cargo\bin;$env:USERPROFILE\.cargo\bin;C:\Windows\System32\config\systemprofile\.cargo\bin;" + $env:Path
Set-Location C:\src
"start" | Out-File C:\build.log
& powershell -ExecutionPolicy Bypass -File C:\src\contrib\windows\build-apo.ps1 *>> C:\build.log
"apo exit $LASTEXITCODE" | Out-File C:\build.log -Append
cargo build --release -p resonance-daemon *>> C:\build.log
"daemon exit $LASTEXITCODE" | Out-File C:\build.log -Append
"BUILD DONE" | Out-File C:\build.log -Append
