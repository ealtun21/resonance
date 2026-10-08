# Windows VM spike (2026-10-07), reproducible pieces

Host: quickemu, Windows 10 Enterprise LTSC 2021 evaluation ISO
(`https://go.microsoft.com/fwlink/p/?LinkID=2195404`; quickget cannot fetch the Windows 11 ISO because
Microsoft blocks it). `autounattend.xml` is quickget's file with the product keys removed, a
`International-Core-WinPE` block (en-US) and `/IMAGE/INDEX` 1 added; rebuild `unattended.iso` with
`genisoimage -J`. Run `quickemu --vm windows-11.conf --display none --ssh-port 22220`.

Guest provisioning order: OpenSSH (Win32-OpenSSH release zip, run `install-sshd.ps1` with
`-ExecutionPolicy Bypass`; the FoD capability does not install), `tools.ps1` (VS Build Tools + rustup,
as SYSTEM via `schtasks`), `build.ps1`, `sign.ps1` (re-sign Scream's catalog with a local cert and
enable test-signing, then reboot), `Install-x64.bat`, `install-apo.ps1`, `setfmt.ps1` (endpoint format
through the MMDevices registry value, as SYSTEM), `run.ps1` + `play.rs` (`resonance-cli` example) to
play a raw f32 file while `resonanced --measure-loopback` records.

`wssh` / `runsys.sh` are the host-side ssh helpers.
