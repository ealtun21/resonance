param([int]$Ch, [int]$Rate, [int]$Mask, [string]$Guid = "{aeec7bce-53f9-4a27-87fc-ae4e0583bf82}")
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

$sub = "SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\$Guid\Properties"
Grant-Key $sub
$k = "HKLM:\$sub"
$name = '{f19f064d-082c-4e27-bc73-6882a1bb8e4c},0'
$b = [byte[]](Get-ItemProperty $k).$name
function Put([int]$off, [uint32]$v, [int]$n) { for ($i = 0; $i -lt $n; $i++) { $b[$off + $i] = ($v -shr (8 * $i)) -band 0xff } }
Put 10 $Ch 2
Put 12 $Rate 4
Put 16 ($Rate * $Ch * 4) 4
Put 20 ($Ch * 4) 2
Put 28 $Mask 4
Set-ItemProperty -Path $k -Name $name -Value $b -Type Binary
Restart-Service AudioEndpointBuilder -Force
Start-Sleep 2
Start-Service Audiosrv
"set ch=$Ch rate=$Rate mask=$Mask"
