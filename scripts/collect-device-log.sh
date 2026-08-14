#!/usr/bin/env sh
# Run this on the Linux or macOS computer connected to the tablet.
set -eu

device="10.11.99.1"
output_directory="."
include_system_log=0
script_directory="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

usage() {
    cat <<'EOF'
Usage: collect-device-log.sh [--device IP] [--output DIRECTORY] [--include-system-log]

Run this script on your Linux or macOS computer, not on the reMarkable.
It streams one archive through one SSH session, so the SSH password is
requested only once.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --device)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            device="$2"
            shift 2
            ;;
        --output)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
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

for command in ssh tar; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "error: $command is required on this computer" >&2
        exit 1
    fi
done

payload_path="$script_directory/device-diagnostics-remote.sh"
if [ ! -r "$payload_path" ]; then
    echo "error: missing $payload_path; keep the diagnostics bundle files together" >&2
    exit 1
fi

stamp="$(date +%Y%m%d-%H%M%S)"
name="calendar-notes-diagnostics-$stamp"
destination="$output_directory/$name"
archive="$output_directory/$name.tar.gz"
mkdir -p "$destination"

echo "Connecting to root@$device (one SSH password prompt)..."
# Feed the diagnostic script to a remote `sh -s` over ssh's stdin (no
# `base64` on the tablet), and capture the raw .tar.gz it streams back on
# stdout.
if ! ssh -o ConnectTimeout=30 "root@$device" \
    "INCLUDE_SYSTEM_LOG=$include_system_log sh -s" <"$payload_path" >"$archive"; then
    echo "error: diagnostics collection failed" >&2
    echo "Verify USB/Wi-Fi connectivity and the tablet SSH password." >&2
    exit 1
fi

if ! tar -xzf "$archive" -C "$destination"; then
    echo "error: downloaded archive could not be extracted: $archive" >&2
    exit 1
fi

echo "Diagnostics copied to $destination"
echo "Shareable archive created at $archive"
