; Resonance Windows installer (Inno Setup 6).
; Built by CI (.github/workflows/windows.yml): release binaries in target\release
; and the APO DLL (built with a static CRT) are staged next to this script first.
;
; Resonance processes audio with an in-graph Audio Processing Object (APO) that
; the Windows audio engine loads on the active playback device. No virtual cable,
; no kernel driver, no reboot, no manual default-device change.

#define AppName "Resonance"
#ifndef AppVersion
  #define AppVersion "0.5.5"
#endif
#define AppPublisher "Resonance"
#define AppExe "resonance-gui.exe"

[Setup]
AppId={{E3B7C2A1-9F4D-4C6E-8B2A-1D5F7A9C0E11}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={autopf}\Resonance
DefaultGroupName=Resonance
UninstallDisplayIcon={app}\resonance.ico
OutputDir=Output
OutputBaseFilename=resonance-setup
SetupIconFile=resonance.ico
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Tasks]
Name: "autostart"; Description: "Start Resonance automatically at logon"; GroupDescription: "Startup:"
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "..\..\target\release\resonanced.exe";    DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\release\resonance.exe";      DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\release\resonance-tui.exe";  DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\release\resonance-gui.exe";  DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\release\resonance-tray.exe"; DestDir: "{app}"; Flags: ignoreversion
; The APO DLL is loaded by audiodg.exe from this fixed path (see InprocServer32).
Source: "..\..\target\release\resonance_apo.dll";  DestDir: "{app}"; Flags: ignoreversion
Source: "resonance.ico";                           DestDir: "{app}"; Flags: ignoreversion
Source: "install-apo.ps1";                         DestDir: "{app}"; Flags: ignoreversion
Source: "uninstall-apo.ps1";                       DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Resonance";        Filename: "{app}\{#AppExe}"; IconFilename: "{app}\resonance.ico"
Name: "{group}\Uninstall Resonance"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Resonance";  Filename: "{app}\{#AppExe}"; IconFilename: "{app}\resonance.ico"; Tasks: desktopicon

[Registry]
; Autostart runs ONLY the daemon (GUI-subsystem, no console window) via the same
; HKCU Run value name ("Resonance") that resonance_ipc::service manages — so
; toggling autostart from the GUI/TUI modifies exactly what the installer set.
; uninsdeletevalue removes it on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; \
    ValueType: string; ValueName: "Resonance"; ValueData: """{app}\resonanced.exe"""; \
    Tasks: autostart; Flags: uninsdeletevalue

[Run]
; Register the APO, attach it to the active playback device(s), and restart the
; audio engine so it takes effect immediately (no reboot).
Filename: "powershell.exe"; \
    Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\install-apo.ps1"" -DllPath ""{app}\resonance_apo.dll"""; \
    StatusMsg: "Installing the Resonance audio processor..."; Flags: runhidden waituntilterminated
Filename: "{app}\{#AppExe}"; Description: "Launch Resonance now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Restore the audio device's original effect chain and unregister the APO.
Filename: "powershell.exe"; \
    Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\uninstall-apo.ps1"""; \
    Flags: runhidden waituntilterminated; RunOnceId: "RemoveApo"

[Code]
// The APO DLL may be loaded in audiodg.exe (the audio engine) from a prior
// install, which would block overwriting it ("DeleteFile failed; Access is
// denied"). Stop the audio service before copying files so the DLL is free;
// install-apo.ps1 restarts it afterward.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  // Close the running daemon/clients so their .exe files can be replaced.
  Exec('taskkill.exe',
    '/f /im resonanced.exe /im resonance-gui.exe /im resonance-tui.exe /im resonance.exe',
    '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  // Stop the audio engine so audiodg.exe releases the APO DLL.
  Exec('net.exe', 'stop audiosrv /y', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;
