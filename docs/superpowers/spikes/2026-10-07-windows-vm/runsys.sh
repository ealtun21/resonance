#!/bin/sh
# usage: runsys.sh "<powershell file + args>"  -- runs as SYSTEM, waits, prints C:\sys.out
/tmp/wssh "del C:\\sys.out 2>nul & schtasks /create /tn sysrun /sc once /st 23:59 /ru SYSTEM /tr \"cmd /c powershell -ExecutionPolicy Bypass -File $1 > C:\\sys.out 2>&1\" /f >nul & schtasks /run /tn sysrun >nul" 2>&1 | grep -v Warning
sleep ${2:-15}
/tmp/wssh 'type C:\sys.out' 2>&1 | grep -v Warning
