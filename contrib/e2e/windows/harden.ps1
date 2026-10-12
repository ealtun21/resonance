# Make the evaluation guest safe for runs longer than an hour. Run as SYSTEM (wlms is protected).
#  - The eval licence is long expired and `wlms` (Windows Licensing Monitoring Service) powers the
#    guest off one hour after every boot, killing any run that outlasts it. Disabling it only
#    takes effect on the next boot (the running service is critical: killing it bluescreens).
#  - `w32time` steps the clock by hours ~30 min after boot (the RTC is the host's local time read
#    in the guest's timezone), which breaks cargo's mtime freshness checks and long timings.
# Log: C:\harden.log, ends with "HARDEN DONE".
sc.exe config wlms start= disabled *> C:\harden.log
Stop-Service w32time -Force -EA 0; Set-Service w32time -StartupType Disabled -EA 0
sc.exe qc wlms *>> C:\harden.log
"HARDEN DONE" | Out-File C:\harden.log -Append
