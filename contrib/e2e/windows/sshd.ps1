# FirstLogon: install OpenSSH server from the Win32-OpenSSH release (the Windows feature-on-demand
# capability does not install on this evaluation image), authorise the e2e key, start audio.
# Lives in the root of the answer-file ISO (found by drive letter); `cargo xtask e2e image
# windows` substitutes @SSH_PUBKEY@.
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$log = 'C:\sshd-setup.log'
Start-Transcript -Path $log -Force | Out-Null
for ($i = 0; $i -lt 30 -and -not (Test-Connection -Quiet -Count 1 github.com); $i++) { Start-Sleep 10 }
Invoke-WebRequest -UseBasicParsing 'https://github.com/PowerShell/Win32-OpenSSH/releases/latest/download/OpenSSH-Win64.zip' -OutFile C:\OpenSSH-Win64.zip
Expand-Archive -Force C:\OpenSSH-Win64.zip 'C:\Program Files'
& powershell -NoProfile -ExecutionPolicy Bypass -File 'C:\Program Files\OpenSSH-Win64\install-sshd.ps1'
Set-Service sshd -StartupType Automatic
Start-Service sshd
New-NetFirewallRule -Name sshd -DisplayName sshd -Enabled True -Direction Inbound -Protocol TCP -LocalPort 22 -Action Allow -ErrorAction SilentlyContinue | Out-Null
New-Item -ItemType Directory -Force C:\ProgramData\ssh | Out-Null
Set-Content -Path C:\ProgramData\ssh\administrators_authorized_keys -Value '@SSH_PUBKEY@'
icacls C:\ProgramData\ssh\administrators_authorized_keys /inheritance:r /grant 'Administrators:F' 'SYSTEM:F'
Set-Service Audiosrv -StartupType Automatic
Start-Service Audiosrv
Write-Output "sshd-setup done"
Stop-Transcript | Out-Null
