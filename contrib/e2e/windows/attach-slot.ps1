# Attach the Resonance APO to ONE slot of the endpoint named $Name, removing it from all others.
# Scream (legacy WDM, no effect modes) only loads GFX (slot 2); see spec section 14.
param([string]$Slot = "2", [string]$Name = "Scream")
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

$g = $null
foreach ($e in Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render') {
  $n = (Get-ItemProperty "$($e.PSPath)\Properties" -EA SilentlyContinue).'{b3f8fa53-0004-438e-9003-51a46e139bfc},6'
  if ($n -like "*$Name*") { $g = $e.PSChildName; break }
}
if (-not $g) { throw "no render endpoint named *$Name*" }
$sub="SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\$g\FxProperties"
Grant-Key $sub
$fxp="HKLM:\$sub"
$fx='{D04E05A6-594B-4FB6-A80D-01AF5EED7D1D}'; $mode='{D3993A3F-99C2-4402-B5EC-A92A0367664B}'
$clsid='{7C3D2A1E-9B6F-4E2A-8D5C-1F0A3B4C5D6E}'; $dm='{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}'
foreach($s in '1','2','5','6','7'){ Remove-ItemProperty -Path $fxp -Name "$fx,$s" -EA SilentlyContinue; Remove-ItemProperty -Path $fxp -Name "$mode,$s" -EA SilentlyContinue }
Set-ItemProperty -Path $fxp -Name "$fx,$Slot" -Value $clsid
New-ItemProperty -Force -Path $fxp -Name "$mode,$Slot" -PropertyType MultiString -Value @($dm) | Out-Null
Restart-Service AudioEndpointBuilder -Force; Start-Sleep 2; Start-Service Audiosrv
"slot=$Slot"
