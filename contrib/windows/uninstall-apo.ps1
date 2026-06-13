# Remove the Resonance APO: restore each render endpoint's original effect
# slots, unregister the COM class + APO catalog, and clear the signature-check
# override. Run elevated (Inno Setup [UninstallRun]).
$ErrorActionPreference = 'Continue'

# Close the running daemon/clients so the uninstaller can delete their .exe.
taskkill /f /im resonanced.exe /im resonance-gui.exe /im resonance-tui.exe /im resonance.exe 2>$null | Out-Null

$clsid = '{7C3D2A1E-9B6F-4E2A-8D5C-1F0A3B4C5D6E}'
$backupRoot = 'HKLM:\SOFTWARE\Resonance\ApoBackup'

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

$renderSub = 'SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render'
$render = "HKLM:\$renderSub"
$fx   = '{D04E05A6-594B-4FB6-A80D-01AF5EED7D1D}'
$mode = '{D3993A3F-99C2-4402-B5EC-A92A0367664B}'

# Read the saved prior signature-check state before the backup tree is removed,
# so we can decide whether to clear DisableProtectedAudioDG below.
$priorDpadg = (Get-ItemProperty -Path $backupRoot -Name 'DisableProtectedAudioDG_prior' -EA SilentlyContinue).'DisableProtectedAudioDG_prior'

# restore each endpoint we touched from the backup
if (Test-Path $backupRoot) {
  Get-ChildItem $backupRoot -EA SilentlyContinue | ForEach-Object {
    $g = $_.PSChildName
    $fxp = "$render\$g\FxProperties"
    if (-not (Test-Path $fxp)) { return }
    foreach ($slot in '1','2','5','6','7') {
      $name = "$fx,$slot"
      $orig = (Get-ItemProperty -Path $_.PSPath -Name $name -EA SilentlyContinue).$name
      try {
        if ($null -eq $orig -or $orig -eq '<none>') {
          Remove-ItemProperty -Path $fxp -Name $name -EA SilentlyContinue
          Remove-ItemProperty -Path $fxp -Name "$mode,$slot" -EA SilentlyContinue
        } else {
          Set-ItemProperty -Path $fxp -Name $name -Value $orig
        }
      } catch {}
    }
  }
  Remove-Item -Recurse -Force $backupRoot -EA SilentlyContinue
}

# unregister COM class + APO catalog
Remove-Item -Recurse -Force "HKLM:\SOFTWARE\Classes\CLSID\$clsid" -EA SilentlyContinue
Remove-Item -Recurse -Force "HKLM:\SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\$clsid" -EA SilentlyContinue

# Restore the signature-check override to its pre-install state. Only clear it
# if it didn't exist before we installed ('<none>' or no marker) — otherwise
# another unsigned APO (e.g. EqualizerAPO) set it and still needs it.
$audioKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio'
if ($null -eq $priorDpadg -or $priorDpadg -eq '<none>') {
  Remove-ItemProperty -Path $audioKey -Name 'DisableProtectedAudioDG' -EA SilentlyContinue
} else {
  Set-ItemProperty -Path $audioKey -Name 'DisableProtectedAudioDG' -Value ([int]$priorDpadg) -Type DWord
}

# drop the daemon's shared state file
Remove-Item -Force 'C:\ProgramData\Resonance\apo_state.bin' -EA SilentlyContinue

Restart-Service audiosrv -Force -EA SilentlyContinue
Write-Output 'Resonance APO removed.'
