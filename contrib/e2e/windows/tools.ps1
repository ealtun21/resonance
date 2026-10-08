$ProgressPreference = 'SilentlyContinue'
Set-Location C:\
curl.exe -sL -o C:\vs_BuildTools.exe https://aka.ms/vs/17/release/vs_BuildTools.exe
curl.exe -sL -o C:\rustup-init.exe https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe
"downloaded" | Out-File C:\tools.log
Start-Process -Wait C:\vs_BuildTools.exe -ArgumentList '--quiet','--wait','--norestart','--nocache','--add','Microsoft.VisualStudio.Workload.VCTools','--includeRecommended'
"vs done $LASTEXITCODE" | Out-File C:\tools.log -Append
Start-Process -Wait C:\rustup-init.exe -ArgumentList '-y','--default-toolchain','1.99.0','--profile','minimal'
"rust done" | Out-File C:\tools.log -Append
