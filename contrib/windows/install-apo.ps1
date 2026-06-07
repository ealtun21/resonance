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
foreach ($kv in @{MajorVersion=0;MinorVersion=5;MinInputConnections=1;MaxInputConnections=1;MinOutputConnections=1;MaxOutputConnections=1;MaxInstances=4294967295;Flags=15;NumAPOInterfaces=1}.GetEnumerator()) {
  New-ItemProperty -Force -Path $cat -Name $kv.Key -PropertyType DWord -Value $kv.Value | Out-Null
}
Set-ItemProperty -Path $cat -Name 'APOInterface0' -Value '{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}'

# --- 3) disable APO signature check so the unsigned APO loads in audiodg ---
$audioKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio'
if (-not (Test-Path $audioKey)) { New-Item -Path $audioKey -Force | Out-Null }
Set-ItemProperty -Path $audioKey -Name 'DisableProtectedAudioDG' -Value 1 -Type DWord

# --- 4) attach to each render endpoint, ONE mode per endpoint ---
# CRITICAL: do NOT write all five FX slots. The audio engine instantiates the
# APO once per populated slot, so writing legacy (LFX ,1 / GFX ,2) AND modern
# (SFX ,5 / MFX ,6 / EFX ,7) cascades the effect 2+ times and, on some endpoints
# (e.g. a headphone jack), produces an invalid mix-graph the engine bypasses
# entirely (audio plays, APO sees silence, no EQ). Mirror EqualizerAPO: pick a
# single mode per endpoint - modern default is SFX(5)+EFX(7); a driver that only
# ships legacy APOs gets LFX(1)+GFX(2); a combined/Bluetooth endpoint gets
# SFX(5)+MFX(6) (EFX doesn't apply to combined streams). Slots not in the chosen
# mode are DELETED (also cleans up the old all-five pollution from <=0.5.2).
$renderSub = 'SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render'
$render = "HKLM:\$renderSub"
$fx   = '{D04E05A6-594B-4FB6-A80D-01AF5EED7D1D}'   # PKEY_FX_*  ,1/,2 legacy  ,5/,6/,7 SFX/MFX/EFX
$mode = '{D3993A3F-99C2-4402-B5EC-A92A0367664B}'   # per-slot processing-mode list
$defaultMode = '{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}'   # DEFAULT processing mode
$combinedName = '{b3f8fa53-0004-438e-9003-51a46e139bfc},41'   # combined-device flag
New-Item -Force -Path $backupRoot | Out-Null
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
    if ($hasLegacy -and -not $hasModern) { $writeSlots = '1','2' }
    elseif ($combined)                   { $writeSlots = '5','6' }
    else                                 { $writeSlots = '5','7' }

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
