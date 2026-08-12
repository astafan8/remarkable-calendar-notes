<#
.SYNOPSIS
Collect Calendar Notes diagnostics using one SSH password prompt.

.DESCRIPTION
Run this script on the Windows computer connected to the reMarkable. It
streams one diagnostics archive through one SSH session and still collects
AppLoad/xochitl launch errors when the application created no log.
#>
param(
    [string]$Device = "10.11.99.1",
    [string]$OutputDirectory = ".",
    [switch]$IncludeSystemLog
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

foreach ($command in @("ssh", "tar")) {
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "$command is required. Install Windows OpenSSH Client in Settings > Optional Features."
    }
}

$payloadPath = Join-Path $PSScriptRoot "device-diagnostics-remote.sh"
if (-not (Test-Path $payloadPath)) {
    throw "Missing $payloadPath. Keep the diagnostics bundle files together."
}

$payloadText = (Get-Content $payloadPath -Raw) -replace "`r`n", "`n"
$payload = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($payloadText))
$include = if ($IncludeSystemLog) { "1" } else { "0" }
$remoteCommand = "printf '%s' '$payload' | base64 -d | INCLUDE_SYSTEM_LOG=$include OUTPUT_ENCODING=base64 sh"

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$name = "calendar-notes-diagnostics-$stamp"
$destination = Join-Path $OutputDirectory $name
$archive = Join-Path $OutputDirectory "$name.tar.gz"
New-Item -ItemType Directory -Force -Path $destination | Out-Null

Write-Host "Connecting to root@$Device (one SSH password prompt)..."
$encodedArchive = & ssh -o ConnectTimeout=10 "root@$Device" $remoteCommand
if ($LASTEXITCODE -ne 0) {
    throw "Diagnostics collection failed. Verify USB/Wi-Fi connectivity and the tablet SSH password."
}

$encoded = ($encodedArchive -join "").Trim()
if (-not $encoded) {
    throw "The tablet returned an empty diagnostics archive."
}
try {
    [IO.File]::WriteAllBytes($archive, [Convert]::FromBase64String($encoded))
} catch {
    throw "The tablet returned invalid diagnostics data: $($_.Exception.Message)"
}

& tar -xzf $archive -C $destination
if ($LASTEXITCODE -ne 0) {
    throw "The archive was downloaded but could not be extracted: $archive"
}

Write-Host "Diagnostics copied to $destination"
Write-Host "Shareable archive created at $archive"
