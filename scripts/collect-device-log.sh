#!/usr/bin/env sh
# Run this on the Linux or macOS computer connected to the tablet.
set -eu

device="10.11.99.1"
output_directory="."
include_system_log=0

usage() {
    cat <<'EOF'
Usage: collect-device-log.sh [--device IP] [--output DIRECTORY] [--include-system-log]

Run this script on your Linux or macOS computer, not on the reMarkable.
It uses ssh/scp to fetch Calendar Notes diagnostics from the tablet.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --device)
            device="$2"
            shift 2
            ;;
        --output)
            output_directory="$2"
            shift 2
            ;;
        --include-system-log)
            include_system_log=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

for command in ssh scp tar; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "error: $command is required on this computer" >&2
        exit 1
    fi
done

stamp="$(date +%Y%m%d-%H%M%S)"
name="calendar-notes-diagnostics-$stamp"
destination="$output_directory/$name"
mkdir -p "$destination"

if ! ssh -o ConnectTimeout=10 "root@$device" true; then
    echo "error: cannot connect to root@$device" >&2
    echo "Connect the tablet by USB, verify its SSH password, or pass its Wi-Fi IP with --device." >&2
    exit 1
fi

remote_file_exists() {
    ssh "root@$device" "test -r '$1'"
}

copy_remote_file() {
    remote_path="$1"
    local_name="$2"
    if ! scp "root@$device:$remote_path" "$destination/$local_name"; then
        echo "error: failed to copy $remote_path from $device" >&2
        exit 1
    fi
}

persistent_log="/home/root/.local/share/remarkable-calendar-notes/calendar-notes.log"
temporary_log="/tmp/calendar-notes.log"
if remote_file_exists "$persistent_log"; then
    copy_remote_file "$persistent_log" "calendar-notes.log"
elif remote_file_exists "$temporary_log"; then
    copy_remote_file "$temporary_log" "calendar-notes.log"
else
    echo "error: no Calendar Notes log was found" >&2
    echo "Launch v0.1.5 or newer once, then run this script again." >&2
    exit 1
fi

previous_log="/home/root/.local/share/remarkable-calendar-notes/calendar-notes.previous.log"
if remote_file_exists "$previous_log"; then
    copy_remote_file "$previous_log" "calendar-notes.previous.log"
fi

if ! ssh "root@$device" \
    'printf "kernel: "; uname -a; printf "os-version: "; cat /etc/version 2>/dev/null || true; printf "manifest: "; grep -m1 version /home/root/xovi/exthome/appload/remarkable-calendar-notes/external.manifest.json 2>/dev/null || true; printf "qtfb-socket: "; if test -S /tmp/qtfb.sock; then echo present; else echo missing; fi' \
    >"$destination/device-info.txt"; then
    echo "Device information could not be collected." >"$destination/device-info.txt"
fi

if [ "$include_system_log" -eq 1 ]; then
    if ! ssh "root@$device" \
        'journalctl -u xochitl --since "-10 minutes" --no-pager 2>&1' \
        >"$destination/xochitl-last-10-minutes.log"; then
        echo "warning: optional xochitl system log could not be collected" >&2
    fi
fi

cat >"$destination/collection-info.txt" <<EOF
Collected from: $device
Collected at: $(date -u +%Y-%m-%dT%H:%M:%SZ)
Collector: Linux/macOS host shell
System log included: $include_system_log

The app log excludes calendar contents, source URLs, credentials, and tokens.
The optional xochitl system log may contain unrelated device diagnostics;
review it before sharing.
EOF

archive="$output_directory/$name.tar.gz"
tar -czf "$archive" -C "$output_directory" "$name"
echo "Diagnostics copied to $destination"
echo "Shareable archive created at $archive"
