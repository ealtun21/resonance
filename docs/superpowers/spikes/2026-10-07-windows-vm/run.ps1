param([string]$In = 'C:\stim8.raw', [int]$Secs = 8)
Remove-Item C:\out.raw -ErrorAction SilentlyContinue
$cap = Start-Process -PassThru -WindowStyle Hidden 'C:\Program Files\Resonance\resonanced.exe' -ArgumentList '--measure-loopback','Scream','C:\out.raw',"$Secs" -RedirectStandardOutput C:\cap.out -RedirectStandardError C:\cap.err
Start-Sleep 2
& C:\src\target\release\examples\play.exe $In 2>&1 | ForEach-Object { "$_" }
$cap.WaitForExit()
Get-Content C:\cap.err, C:\cap.out -ErrorAction SilentlyContinue
(Get-Item C:\out.raw).Length
