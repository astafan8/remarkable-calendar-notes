<#
.SYNOPSIS
Collect Calendar Notes diagnostics from a reMarkable tablet.

.DESCRIPTION
Run this script in PowerShell on the Windows computer connected to the
tablet. It uses SSH/SCP to fetch the app log; it does not run on the tablet.
#>
param(
    [string]$Device = "10.11.99.1",
    [string]$OutputDirectory = ".",
    [switch]$IncludeSystemLog
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false
$remoteDataDirectory = "/home/root/.local/share/remarkable-calendar-notes"
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$name = "calendar-notes-diagnostics-$stamp"
$destination = Join-Path $OutputDirectory $name

foreach ($command in @("ssh", "scp")) {
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "$command is required. Install Windows OpenSSH Client in Settings > Optional Features."
    }
}

function Test-RemoteFile {
    param([string]$Path)

    & ssh "root@$Device" "test -r '$Path'"
    return $LASTEXITCODE -eq 0
}

function Copy-RemoteFile {
    param(
        [string]$RemotePath,
        [string]$LocalName
    )

    & scp "root@${Device}:${RemotePath}" (Join-Path $destination $LocalName)
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to copy $RemotePath from $Device."
    }
}

New-Item -ItemType Directory -Force -Path $destination | Out-Null

& ssh -o ConnectTimeout=10 "root@$Device" "true"
if ($LASTEXITCODE -ne 0) {
    throw "Cannot connect to root@$Device. Connect the tablet by USB, verify its SSH password, or pass its Wi-Fi IP with -Device."
}

$persistentLog = "$remoteDataDirectory/calendar-notes.log"
$temporaryLog = "/tmp/calendar-notes.log"
if (Test-RemoteFile $persistentLog) {
    Copy-RemoteFile $persistentLog "calendar-notes.log"
} elseif (Test-RemoteFile $temporaryLog) {
    Copy-RemoteFile $temporaryLog "calendar-notes.log"
} else {
    throw "No Calendar Notes log was found. Launch v0.1.5 or newer once, then run this script again."
}

$previousLog = "$remoteDataDirectory/calendar-notes.previous.log"
if (Test-RemoteFile $previousLog) {
    Copy-RemoteFile $previousLog "calendar-notes.previous.log"
}

$deviceInfo = Join-Path $destination "device-info.txt"
$deviceInfoCommand = 'printf "kernel: "; uname -a; printf "os-version: "; cat /etc/version 2>/dev/null || true; printf "manifest: "; grep -m1 version /home/root/xovi/exthome/appload/remarkable-calendar-notes/external.manifest.json 2>/dev/null || true; printf "qtfb-socket: "; if test -S /tmp/qtfb.sock; then echo present; else echo missing; fi'
& ssh "root@$Device" $deviceInfoCommand |
    Set-Content -Encoding utf8 $deviceInfo
if ($LASTEXITCODE -ne 0) {
    "Device information could not be collected." | Set-Content -Encoding utf8 $deviceInfo
}

if ($IncludeSystemLog) {
    & ssh "root@$Device" 'journalctl -u xochitl --since "-10 minutes" --no-pager 2>&1' |
        Set-Content -Encoding utf8 (Join-Path $destination "xochitl-last-10-minutes.log")
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "The optional xochitl system log could not be collected."
    }
}

@"
Collected from: $Device
Collected at: $(Get-Date -Format o)
Collector: Windows host PowerShell
System log included: $IncludeSystemLog

The app log excludes calendar contents, source URLs, credentials, and tokens.
The optional xochitl system log may contain unrelated device diagnostics;
review it before sharing.
"@ | Set-Content -Encoding utf8 (Join-Path $destination "collection-info.txt")

$archive = Join-Path $OutputDirectory "$name.zip"
Compress-Archive -Path (Join-Path $destination "*") -DestinationPath $archive -Force
Write-Host "Diagnostics copied to $destination"
Write-Host "Shareable archive created at $archive"
