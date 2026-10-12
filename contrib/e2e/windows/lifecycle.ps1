# Lifecycle of the INSTALLED product in the Windows guest: installer -> APO in the audio path ->
# daemon crash / graceful exit / restart -> upgrade over a running install -> uninstall, with the
# audio asserted at every step by `lifecycle-probe.ps1` (a real WASAPI tone + the endpoint's
# post-APO loopback, so "EQ'd" vs "bypassed" is measured, not assumed).
#
# Run as the autologon admin user over ssh, after `lifecycle-build.ps1` produced
# C:\lc\setup-old.exe and C:\lc\setup-new.exe (same binaries, two versions) and C:\lc\probe.ps1.
# The daemon and clients are launched into the interactive session (session 1) through a /it
# scheduled task, exactly as the Run key / the installer's "Launch" would. Exit code = failures.
$ErrorActionPreference = 'Continue'
$inst = 'C:\Program Files\Resonance'
$cli = "$inst\resonance.exe"
$fx = '{D04E05A6-594B-4FB6-A80D-01AF5EED7D1D}'; $mode = '{D3993A3F-99C2-4402-B5EC-A92A0367664B}'
$clsid = '{7C3D2A1E-9B6F-4E2A-8D5C-1F0A3B4C5D6E}'
$render = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render'
$fails = 0
function Check([string]$name, [bool]$ok, [string]$detail = '') {
  if (-not $ok) { $script:fails++ }
  "{0} {1}{2}" -f ($(if ($ok) { 'PASS' } else { 'FAIL' })), $name, $(if ($detail) { "  ($detail)" } else { '' })
}
function Procs { (Get-Process resonance* -EA 0 | % Name | sort) -join ',' }
function Launch([string]$exe) {   # into session 1, elevated, like a logon Run entry
  schtasks /delete /tn lcia /f 2>&1 | Out-Null
  schtasks /create /tn lcia /sc once /st 23:59 /ru Quickemu /it /rl highest /tr $exe /f | Out-Null
  schtasks /run /tn lcia | Out-Null
}
function App([string]$name) { Launch "C:\PROGRA~1\Resonance\$name.exe" }   # 8.3 path: schtasks /tr cannot take a quoted one
function Probe([string]$expect) { $o = & C:\lc\probe.ps1 -Expect $expect; $ok = ($LASTEXITCODE -eq 0); Check "audio is $expect" $ok ($o | Select-Object -Last 1) }
function WaitDaemon { 1..20 | % { if (Get-Process resonanced -EA 0) { return }; Start-Sleep 1 }; Start-Sleep 3 }
function Install([string]$setup) { (Start-Process $setup -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/TASKS=autostart' -Wait -PassThru).ExitCode }
# What the install changes about the machine: every endpoint's FX values + the global switches.
function State {
  Get-ChildItem $render | % {
    "[$($_.PSChildName)]"
    reg query ($_.Name + '\FxProperties') 2>&1 | ? { $_ -match '^\s+\{' } | sort
  }
  "DisableProtectedAudioDG=" + (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio' -EA 0).DisableProtectedAudioDG
  "clsid=" + (Test-Path "HKLM:\SOFTWARE\Classes\CLSID\$clsid") + " catalog=" + (Test-Path "HKLM:\SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\$clsid")
  "Run=" + ((Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -EA 0).PSObject.Properties | ? Name -like 'Resonance*' | % Name) -join ','
}
function Backups { reg query 'HKLM\SOFTWARE\Resonance\ApoBackup' /s 2>&1 | ? { $_ -match 'REG_SZ' } | sort }
function SlotsHoldingApo { Get-ChildItem $render | % { $k = Get-ItemProperty "$($_.PSPath)\FxProperties" -EA 0; $n = ($k.PSObject.Properties | ? { $_.Value -eq $clsid }).Count; if ($n) { $n } } }

"== reset: no Resonance, HDA endpoint, test EQ file"
Stop-Process -Name resonance* -Force -EA 0
if (Test-Path "$inst\unins000.exe") { Start-Process "$inst\unins000.exe" -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES' -Wait }
& powershell -NoProfile -ExecutionPolicy Bypass -File C:\src\contrib\windows\uninstall-apo.ps1 | Out-Null   # the e2e-prepared APO has no uninstaller
& powershell -NoProfile -ExecutionPolicy Bypass -File C:\src\contrib\e2e\windows\attach-slot.ps1 -Slot none -Name Scream 2>&1 | Out-Null
Remove-Item -Recurse -Force $inst, C:\ProgramData\Resonance -EA 0
& powershell -NoProfile -ExecutionPolicy Bypass -File C:\src\contrib\e2e\windows\select-endpoint.ps1 -Which hda | Out-Null
Set-Content -Encoding ASCII C:\lc-test.txt "Preamp: -3 dB`r`nFilter 1: ON PK Fc 1000 Hz Gain 6 dB Q 1.0`r`nFilter 2: ON PK Fc 150 Hz Gain -6 dB Q 1.0"
$clean = State

"== install (old build, silent)"
Check 'installer exits 0' ((Install C:\lc\setup-old.exe) -eq 0)
Check 'APO attached to exactly one FX slot per active endpoint' (((SlotsHoldingApo) | ? { $_ -ne 1 }).Count -eq 0 -and (SlotsHoldingApo).Count -ge 1)
Check 'autostart Run key written' ($null -ne (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -EA 0).Resonance)
Check 'silent install starts nothing' ((Procs) -eq '') (Procs)
Probe 'flat'   # APO registered, no daemon yet: must pass audio untouched
$backups = Backups

"== daemon up, EQ applied"
App resonanced; WaitDaemon
& $cli load C:\lc-test.txt | Out-Null; & $cli save lc-eq | Out-Null; & $cli map lc-eq | Out-Null
Probe 'eq'

"== daemon crash (kill -9): audio must fall back to flat, restart must restore the EQ"
Stop-Process -Name resonanced -Force
Start-Sleep 3   # past the APO's 2 s staleness window
Probe 'flat'    # a stream that starts after the crash is bypassed at once
App resonanced; WaitDaemon
Probe 'eq'     # device-mapped profile auto-applied on startup, heartbeat resumed

"== graceful shutdown"
& $cli shutdown | Out-Null; Start-Sleep 3
Check 'daemon exited' ((Procs) -eq '') (Procs)
Probe 'flat'
App resonanced; WaitDaemon
Probe 'eq'

"== audio engine restart"
Restart-Service Audiosrv -Force; Start-Sleep 6
Probe 'eq'

"== upgrade over a running install (daemon + tray + gui up)"
App resonance-tray; Start-Sleep 2
App resonance-gui; Start-Sleep 6
Check 'daemon, tray and gui running before the upgrade' ((Procs) -eq 'resonanced,resonance-gui,resonance-tray') (Procs)
Check 'upgrade installer exits 0' ((Install C:\lc\setup-new.exe) -eq 0)
Check 'upgrade closed every client' ((Procs) -eq '') (Procs)
Check 'upgrade keeps the original-state backups' ((Compare-Object $backups (Backups)).Count -eq 0)
Probe 'flat'   # daemon gone: bypass
App resonanced; WaitDaemon
Probe 'eq'

"== uninstall (silent)"
Check 'uninstaller exits 0' ((Start-Process "$inst\unins000.exe" -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES' -Wait -PassThru).ExitCode -eq 0)
Start-Sleep 2
Check 'uninstall closed every client' ((Procs) -eq '') (Procs)
Check 'program files removed' (-not (Test-Path "$inst\resonanced.exe"))
$after = State
$d = Compare-Object $clean $after
Check 'machine state equals the pre-install state (FX slots, signature override, COM/catalog, Run keys)' (@($d).Count -eq 0) (($d | % { $_.InputObject }) -join ' | ')
Probe 'flat'

"lifecycle: $fails failure(s)"
exit $fails
