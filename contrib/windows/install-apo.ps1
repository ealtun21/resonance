# Install the Resonance Audio Processing Object (APO) into the Windows audio
# engine. No driver, no virtual cable: a user-mode COM DLL that audiodg.exe
# loads as a system-effect APO on the active render endpoints.
#
# Run elevated (the Inno Setup [Run] section does this). Parameters:
#   -DllPath   full path to resonance_apo.dll (installed under {app})
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)] [string] $DllPath
)
$ErrorActionPreference = 'Continue'

# Transcript so the (run-hidden) installer's APO registration + per-endpoint
# attach results are captured - the only way to diagnose attach failures on
# machines we can't reach (real audio drivers, Win editions, AV, etc.).
$logDir = Join-Path $env:ProgramData 'Resonance'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
try { Start-Transcript -Path (Join-Path $logDir 'apo-install.log') -Force | Out-Null } catch {}
Write-Output "install-apo: DllPath=$DllPath exists=$(Test-Path $DllPath) OS=$([Environment]::OSVersion.Version)"

# The daemon (logged-in user) creates %ProgramData%\Resonance\apo_state.bin; the
# APO inside audiodg.exe must also open it READ-WRITE (seqlock + telemetry). When
# audiodg runs as the restricted LocalService token it would get ACCESS_DENIED on
# the user-created file, the APO falls back to flat passthrough, and there's no
# EQ/spectrum. Grant the dir (with inheritance) to LocalService + NetworkService
# + Users so the daemon-created file inherits write access for audiodg.
& icacls.exe "$logDir" /grant '*S-1-5-19:(OI)(CI)F' '*S-1-5-20:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)M' 2>&1 | Out-Null
Write-Output "granted ACLs on $logDir (icacls exit $LASTEXITCODE)"

$clsid = '{7C3D2A1E-9B6F-4E2A-8D5C-1F0A3B4C5D6E}'
$backupRoot = 'HKLM:\SOFTWARE\Resonance\ApoBackup'

# --- privileges needed to take ownership of SYSTEM-owned MMDevices keys ---
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Priv {
  [StructLayout(LayoutKind.Sequential)] public struct LUID { public uint Lo; public int Hi; }
  [StructLayout(LayoutKind.Sequential)] public struct LAA { public LUID Luid; public uint Attr; }
  [StructLayout(LayoutKind.Sequential)] public struct TP { public uint Count; public LAA Priv; }
  [DllImport("advapi32.dll", SetLastError=true)] public static extern bool OpenProcessToken(IntPtr h, uint a, out IntPtr t);
  [DllImport("advapi32.dll", SetLastError=true)] public static extern bool LookupPrivilegeValue(string s, string n, out LUID l);
  [DllImport("advapi32.dll", SetLastError=true)] public static extern bool AdjustTokenPrivileges(IntPtr t, bool d, ref TP n, uint len, IntPtr p, IntPtr r);
  [DllImport("kernel32.dll")] public static extern IntPtr GetCurrentProcess();
  public static void Enable(string name){ IntPtr tok; OpenProcessToken(GetCurrentProcess(),0x28,out tok); LUID l; LookupPrivilegeValue(null,name,out l); TP tp; tp.Count=1; tp.Priv.Luid=l; tp.Priv.Attr=2; AdjustTokenPrivileges(tok,false,ref tp,0,IntPtr.Zero,IntPtr.Zero); }
}
"@
[Priv]::Enable('SeTakeOwnershipPrivilege'); [Priv]::Enable('SeRestorePrivilege'); [Priv]::Enable('SeBackupPrivilege')
$admins = New-Object System.Security.Principal.SecurityIdentifier('S-1-5-32-544')

function Grant-Key([string]$sub) {
  if (-not (Test-Path "HKLM:\$sub")) { return }
  $k = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($sub,
        [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree,
        [System.Security.AccessControl.RegistryRights]::TakeOwnership)
  $a = $k.GetAccessControl([System.Security.AccessControl.AccessControlSections]::None)
  $a.SetOwner($admins); $k.SetAccessControl($a); $k.Close()
  $k = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($sub,
        [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree,
        [System.Security.AccessControl.RegistryRights]::ChangePermissions)
  $a = $k.GetAccessControl()
  $rule = New-Object System.Security.AccessControl.RegistryAccessRule($admins,'FullControl','ContainerInherit','None','Allow')
  $a.AddAccessRule($rule); $k.SetAccessControl($a); $k.Close()
}

# --- 1) COM in-proc server ---
$base = "HKLM:\SOFTWARE\Classes\CLSID\$clsid"
New-Item -Force -Path "$base\InprocServer32" | Out-Null
Set-ItemProperty -Path $base -Name '(default)' -Value 'Resonance Audio Processing Object'
Set-ItemProperty -Path "$base\InprocServer32" -Name '(default)' -Value $DllPath
Set-ItemProperty -Path "$base\InprocServer32" -Name 'ThreadingModel' -Value 'Both'

# --- 2) APO catalog (audiodg ignores FxProperties CLSIDs not registered here) ---
$cat = "HKLM:\SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\$clsid"
New-Item -Force -Path $cat | Out-Null
Set-ItemProperty -Path $cat -Name 'FriendlyName' -Value 'Resonance APO'
Set-ItemProperty -Path $cat -Name 'Copyright'    -Value 'Resonance'
# MaxInstances is "unlimited" = 0xFFFFFFFF. We store it as -1: Windows PowerShell
# 5.1 converts a DWord value through Int32, so the literal 4294967295 overflows
# and the property is silently never written. -1 has the identical 32-bit pattern.
foreach ($kv in @{MajorVersion=0;MinorVersion=5;MinInputConnections=1;MaxInputConnections=1;MinOutputConnections=1;MaxOutputConnections=1;MaxInstances=-1;Flags=15;NumAPOInterfaces=1}.GetEnumerator()) {
  New-ItemProperty -Force -Path $cat -Name $kv.Key -PropertyType DWord -Value $kv.Value | Out-Null
}
Set-ItemProperty -Path $cat -Name 'APOInterface0' -Value '{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}'

# --- 3) disable APO signature check so the unsigned APO loads in audiodg ---
$audioKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio'
if (-not (Test-Path $audioKey)) { New-Item -Path $audioKey -Force | Out-Null }
# Record the prior value (or '<none>') so uninstall only clears the override if
# WE created it — another unsigned APO (e.g. EqualizerAPO) may also rely on it.
$priorDpadg = (Get-ItemProperty -Path $audioKey -Name 'DisableProtectedAudioDG' -EA SilentlyContinue).DisableProtectedAudioDG
Set-ItemProperty -Path $audioKey -Name 'DisableProtectedAudioDG' -Value 1 -Type DWord

# --- 4) attach to each render endpoint, ONE slot per endpoint ---
# CRITICAL: write exactly ONE FX slot. The audio engine instantiates + runs the
# APO once per populated slot, and our chain is a FULL effects/EQ pipeline — so
# any two populated slots apply the whole chain TWICE to the same stream (e.g.
# SFX before the mixer + EFX after it), doubling every gain (+15 dB bass becomes
# +30 dB → clipping/clicks) and mis-shaping the EQ. (Earlier <=0.5.3 wrote the
# *pair* SFX+EFX thinking of it as one "mode"; that double-processed.) Pick a
# single slot by what the driver provides: modern endpoints get EFX(7) — the
# endpoint/post-mix effect, applied exactly once to the final mix (best for the
# limiter); a combined/Bluetooth endpoint (no EFX) gets MFX(6); a legacy-only
# driver gets GFX(2). Every other slot is DELETED (also cleans up old pollution).
$renderSub = 'SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render'
$render = "HKLM:\$renderSub"
$fx   = '{D04E05A6-594B-4FB6-A80D-01AF5EED7D1D}'   # PKEY_FX_*  ,1/,2 legacy  ,5/,6/,7 SFX/MFX/EFX
$mode = '{D3993A3F-99C2-4402-B5EC-A92A0367664B}'   # per-slot processing-mode list
$defaultMode = '{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}'   # DEFAULT processing mode
$combinedName = '{b3f8fa53-0004-438e-9003-51a46e139bfc},41'   # combined-device flag
New-Item -Force -Path $backupRoot | Out-Null
# Persist the prior signature-check state alongside the slot backups.
if ($null -eq $priorDpadg) {
  Set-ItemProperty -Path $backupRoot -Name 'DisableProtectedAudioDG_prior' -Value '<none>'
} else {
  Set-ItemProperty -Path $backupRoot -Name 'DisableProtectedAudioDG_prior' -Value ([string]$priorDpadg)
}
$allSlots = '1','2','5','6','7'

$attached = 0
$verified = 0
Get-ChildItem $render -EA SilentlyContinue | ForEach-Object {
  $g = $_.PSChildName
  try {
    Grant-Key "$renderSub\$g"
    $fxp = "$render\$g\FxProperties"
    if (Test-Path $fxp) { Grant-Key "$renderSub\$g\FxProperties" } else { New-Item -Path $fxp -EA Stop | Out-Null }

    # back up the original slot values (all five) so uninstall can restore them
    $bk = "$backupRoot\$g"
    New-Item -Force -Path $bk | Out-Null
    foreach ($slot in $allSlots) {
      $name = "$fx,$slot"
      $orig = (Get-ItemProperty -Path $fxp -Name $name -EA SilentlyContinue).$name
      if ($null -ne $orig) { Set-ItemProperty -Path $bk -Name $name -Value $orig } else { Set-ItemProperty -Path $bk -Name $name -Value '<none>' }
    }

    # decide the mode from what the driver already provides (EqualizerAPO logic)
    $cur = Get-ItemProperty -Path $fxp -EA SilentlyContinue
    $hasLegacy = ($null -ne $cur."$fx,1") -or ($null -ne $cur."$fx,2")
    $hasModern = ($null -ne $cur."$fx,5") -or ($null -ne $cur."$fx,6") -or ($null -ne $cur."$fx,7")
    $combined = $null -ne (Get-ItemProperty -Path "$render\$g\Properties" -Name $combinedName -EA SilentlyContinue).$combinedName
    # Exactly ONE slot (leading comma forces a single-element array so
    # `-contains` / `[0]` below behave). Two slots would double-process.
    if ($hasLegacy -and -not $hasModern) { $writeSlots = , '2' }   # GFX (legacy, post-mix)
    elseif ($combined)                   { $writeSlots = , '6' }   # MFX (combined; EFX N/A)
    else                                 { $writeSlots = , '7' }   # EFX (endpoint, post-mix)

    foreach ($slot in $allSlots) {
      $name = "$fx,$slot"
      $mname = "$mode,$slot"
      if ($writeSlots -contains $slot) {
        Set-ItemProperty -Path $fxp -Name $name -Value $clsid
        # processing-mode list is REG_MULTI_SZ (DEFAULT mode)
        New-ItemProperty -Force -Path $fxp -Name $mname -PropertyType MultiString -Value @($defaultMode) | Out-Null
      } else {
        if ($null -ne (Get-ItemProperty -Path $fxp -Name $name -EA SilentlyContinue).$name) {
          Remove-ItemProperty -Path $fxp -Name $name -EA SilentlyContinue
        }
        if ($null -ne (Get-ItemProperty -Path $fxp -Name $mname -EA SilentlyContinue).$mname) {
          Remove-ItemProperty -Path $fxp -Name $mname -EA SilentlyContinue
        }
      }
    }
    $attached++
    $rb = (Get-ItemProperty -Path $fxp -Name "$fx,$($writeSlots[0])" -EA SilentlyContinue)."$fx,$($writeSlots[0])"
    if ($rb -eq $clsid) { $verified++; Write-Output "attached+verified $g mode=$($writeSlots -join '+')" }
    else { Write-Output "attached but READ-BACK MISMATCH $g (mode=$($writeSlots -join '+') got '$rb') - endpoint may be vendor-locked" }
  } catch {
    Write-Output "attach FAILED $g : $($_.Exception.Message)"
  }
}
Write-Output "attach summary: attached=$attached verified=$verified"
if ($verified -eq 0) {
  Write-Output 'ERROR: APO did not verify on ANY render endpoint - EQ will not work. The default playback device may be vendor-locked or protected.'
}

# --- 5) restart the audio engine so audiodg reloads APO registrations ---
Restart-Service audiosrv -Force -EA SilentlyContinue
Write-Output 'Resonance APO install finished.'
try { Stop-Transcript | Out-Null } catch {}
