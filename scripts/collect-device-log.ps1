param(
    [string]$Device = "10.11.99.1",
    [string]$OutputDirectory = "."
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false
$remoteDirectory = "/home/root/.local/share/remarkable-calendar-notes"
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$destination = Join-Path $OutputDirectory "calendar-notes-diagnostics-$stamp"
New-Item -ItemType Directory -Force $destination | Out-Null

& scp "root@${Device}:${remoteDirectory}/calendar-notes.log" $destination
if ($LASTEXITCODE -ne 0) {
    throw "Failed to copy calendar-notes.log from $Device"
}
& scp "root@${Device}:${remoteDirectory}/calendar-notes.previous.log" $destination 2>$null
if ($LASTEXITCODE -ne 0) {
    Write-Host "No previous rotated log was present."
}

@"
Collected from: $Device
Collected at: $(Get-Date -Format o)
Release: ask the device owner which Calendar Notes release was installed.
"@ | Set-Content -Encoding utf8 (Join-Path $destination "collection-info.txt")

Write-Host "Diagnostics copied to $destination"
