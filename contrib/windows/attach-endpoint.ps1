# Attach the Resonance APO to ONE render endpoint, by registry GUID. The daemon
# spawns this when it sees a newly-appeared render device, so a hot-plugged DAC
# or a Bluetooth headset gets the EQ WITHOUT re-running the installer. Scope is
# deliberately narrow vs install-apo.ps1: it does NOT register the COM CLSID /
# APO catalog / DisableProtectedAudioDG (those are global, set once at install)
# and does NOT restart audiosrv — a brand-new endpoint's audiodg graph is built
# on its first stream, so writing the FX slot beforehand is picked up with no
# restart (no audio blip for everyone else on a BT connect).
[CmdletBinding()]
param([string] $EndpointGuid = $env:RESONANCE_ATTACH_GUID)
$ErrorActionPreference = 'Continue'
if (-not $EndpointGuid) { Write-Output 'attach-endpoint: no endpoint guid'; exit 1 }

$clsid       = '{7C3D2A1E-9B6F-4E2A-8D5C-1F0A3B4C5D6E}'
$backupRoot  = 'HKLM:\SOFTWARE\Resonance\ApoBackup'
$fx          = '{D04E05A6-594B-4FB6-A80D-01AF5EED7D1D}'   # PKEY_FX_*  ,1/,2 legacy  ,5/,6/,7 SFX/MFX/EFX
$mode        = '{D3993A3F-99C2-4402-B5EC-A92A0367664B}'
$defaultMode = '{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}'
$combinedNm  = '{b3f8fa53-0004-438e-9003-51a46e139bfc},41'
$renderSub   = 'SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render'
$render      = "HKLM:\$renderSub"
$g           = $EndpointGuid
$allSlots    = '1','2','5','6','7'
$fxp         = "$render\$g\FxProperties"

# Fast path: already carries our CLSID in any slot -> nothing to do (cheap, no
# ownership churn on daemon startup re-scans).
$cur0 = Get-ItemProperty -Path $fxp -EA SilentlyContinue
if ($cur0 -and ($allSlots | Where-Object { $cur0."$fx,$_" -eq $clsid })) {
  Write-Output "attach-endpoint: already attached $g"; exit 0
}

# --- privileges to take ownership of SYSTEM-owned MMDevices keys ---
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class PrivAE {
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
[PrivAE]::Enable('SeTakeOwnershipPrivilege'); [PrivAE]::Enable('SeRestorePrivilege'); [PrivAE]::Enable('SeBackupPrivilege')
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

try {
  Grant-Key "$renderSub\$g"
  if (Test-Path $fxp) { Grant-Key "$renderSub\$g\FxProperties" } else { New-Item -Path $fxp -EA Stop | Out-Null }

  # back up the original slots so uninstall-apo can restore them
  $bk = "$backupRoot\$g"
  New-Item -Force -Path $bk | Out-Null
  foreach ($slot in $allSlots) {
    $name = "$fx,$slot"
    $orig = (Get-ItemProperty -Path $fxp -Name $name -EA SilentlyContinue).$name
    if ($null -ne $orig) { Set-ItemProperty -Path $bk -Name $name -Value $orig } else { Set-ItemProperty -Path $bk -Name $name -Value '<none>' }
  }

  # pick exactly ONE slot from what the driver provides (matches install-apo.ps1)
  $cur = Get-ItemProperty -Path $fxp -EA SilentlyContinue
  $hasLegacy = ($null -ne $cur."$fx,1") -or ($null -ne $cur."$fx,2")
  $hasModern = ($null -ne $cur."$fx,5") -or ($null -ne $cur."$fx,6") -or ($null -ne $cur."$fx,7")
  $combined  = $null -ne (Get-ItemProperty -Path "$render\$g\Properties" -Name $combinedNm -EA SilentlyContinue).$combinedNm
  if ($hasLegacy -and -not $hasModern) { $writeSlots = , '2' }
  elseif ($combined)                   { $writeSlots = , '6' }
  else                                 { $writeSlots = , '7' }

  foreach ($slot in $allSlots) {
    $name = "$fx,$slot"; $mname = "$mode,$slot"
    if ($writeSlots -contains $slot) {
      Set-ItemProperty -Path $fxp -Name $name -Value $clsid
      New-ItemProperty -Force -Path $fxp -Name $mname -PropertyType MultiString -Value @($defaultMode) | Out-Null
    } else {
      if ($null -ne (Get-ItemProperty -Path $fxp -Name $name  -EA SilentlyContinue).$name)  { Remove-ItemProperty -Path $fxp -Name $name  -EA SilentlyContinue }
      if ($null -ne (Get-ItemProperty -Path $fxp -Name $mname -EA SilentlyContinue).$mname) { Remove-ItemProperty -Path $fxp -Name $mname -EA SilentlyContinue }
    }
  }
  $rb = (Get-ItemProperty -Path $fxp -Name "$fx,$($writeSlots[0])" -EA SilentlyContinue)."$fx,$($writeSlots[0])"
  if ($rb -eq $clsid) { Write-Output "attach-endpoint: attached+verified $g mode=$($writeSlots -join '+')" }
  else { Write-Output "attach-endpoint: READ-BACK MISMATCH $g got '$rb'" }
} catch {
  Write-Output "attach-endpoint: FAILED $g : $($_.Exception.Message)"
}
