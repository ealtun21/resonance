# Make exactly one of the VM's two playback devices present (so it is the default):
# the 2-channel QEMU HDA device or the 8-channel Scream virtual device.
# usage: select-endpoint.ps1 -Which hda|scream [-Force]   (no-op if already selected unless -Force;
# -Force re-enables the device, which recovers a Scream endpoint a bad format left unavailable)
param([ValidateSet('hda','scream')][string]$Which, [switch]$Force)
$hda = Get-PnpDevice -Class MEDIA | ? InstanceId -like 'HDAUDIO*'
$scream = Get-PnpDevice -Class MEDIA | ? InstanceId -like 'ROOT\MEDIA*'
$on, $off = if ($Which -eq 'hda') { $hda, $scream } else { $scream, $hda }
if (-not $Force -and $on.Status -eq 'OK' -and $off.Status -ne 'OK') { "already $Which"; exit 0 }
$off | Disable-PnpDevice -Confirm:$false
if ($Force) { $on | Disable-PnpDevice -Confirm:$false -EA SilentlyContinue }
$on | Enable-PnpDevice -Confirm:$false
Restart-Service AudioEndpointBuilder -Force
Start-Sleep 2
Start-Service Audiosrv
"selected $Which"
