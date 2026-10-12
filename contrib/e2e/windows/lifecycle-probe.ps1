# Is audio through the endpoint EQ'd (the +6 dB @ 1 kHz / -6 dB @ 150 Hz test curve) or flat?
# Measures with `resonance verify --no-daemon` (a WASAPI tone + the endpoint's post-APO loopback),
# so it works while the daemon is dead. Spread = max-min of the measured level over 150/1000/6000 Hz:
# the test curve gives ~11.7 dB, a bypassed chain ~0.
#   lifecycle-probe.ps1 -Expect eq|flat [-Cli <resonance.exe>] [-Retries 3]
# Prints one line `PROBE expect=.. verdict=.. spread=.. pitch_ok=..`, exits 1 on a mismatch.
param(
  [Parameter(Mandatory)][ValidateSet('eq', 'flat')][string]$Expect,
  [string]$Cli = 'C:\lc\resonance.exe',
  [int]$Retries = 3
)
$verdict = 'error'; $spread = [double]::NaN; $pitch = $false
for ($i = 0; $i -lt $Retries; $i++) {
  $out = & $Cli verify --no-daemon --json --freqs 150,1000,6000 2>$null
  $j = try { $out | Select-Object -Last 1 | ConvertFrom-Json } catch { $null }
  if ($j -and $j.rows) {
    $m = $j.rows | % { $_.measured_db }
    $spread = ($m | Measure-Object -Maximum -Minimum | % { $_.Maximum - $_.Minimum })
    $pitch = -not ($j.rows | ? { -not $_.pitch_ok })
    $verdict = if ($spread -gt 8) { 'eq' } elseif ($spread -lt 1.5) { 'flat' } else { 'odd' }
    if ($verdict -eq $Expect) { break }
  }
  Start-Sleep 2   # the APO's recovery / re-lock can take a moment
}
"PROBE expect=$Expect verdict=$verdict spread=$([math]::Round($spread, 2)) pitch_ok=$pitch"
if ($verdict -ne $Expect -or -not $pitch) { exit 1 }
