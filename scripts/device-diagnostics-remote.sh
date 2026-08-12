#!/bin/sh
# Internal payload executed over one SSH session by the host-side collectors.
set -eu

include_system_log="${INCLUDE_SYSTEM_LOG:-0}"
output_encoding="${OUTPUT_ENCODING:-raw}"
work="$(mktemp -d /tmp/calendar-notes-diagnostics.XXXXXX)"
trap 'rm -rf "$work"' EXIT HUP INT TERM

log_path=""
for candidate in \
    /home/root/.local/share/remarkable-calendar-notes/calendar-notes.log \
    /tmp/calendar-notes.log \
    $(find /home/root -type f -name calendar-notes.log 2>/dev/null); do
    if [ -r "$candidate" ]; then
        log_path="$candidate"
        break
    fi
done

if [ -n "$log_path" ]; then
    cp "$log_path" "$work/calendar-notes.log"
    previous="$(dirname "$log_path")/calendar-notes.previous.log"
    if [ -r "$previous" ]; then
        cp "$previous" "$work/calendar-notes.previous.log"
    fi
else
    cat >"$work/NO_APP_LOG_FOUND.txt" <<'EOF'
Calendar Notes did not create a log. This normally means AppLoad could not
execute the application binary or the process failed before Rust main().
Inspect device-info.txt and appload-xochitl.log in this archive.
EOF
fi

process_marker=/home/root/.local/share/remarkable-calendar-notes/process-started.txt
if [ -r "$process_marker" ]; then
    cp "$process_marker" "$work/process-started.txt"
fi

app_dir="/home/root/xovi/exthome/appload/remarkable-calendar-notes"
binary="$app_dir/remarkable-calendar-notes"
{
    echo "collected-at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "selected-app-log: ${log_path:-none}"
    printf "kernel: "
    uname -a
    printf "os-version: "
    cat /etc/version 2>/dev/null || true
    echo
    echo "app-directory:"
    ls -la "$app_dir" 2>&1 || true
    echo
    if [ -e "$binary" ]; then
        if [ -x "$binary" ]; then
            echo "binary-executable: yes"
        else
            echo "binary-executable: NO"
        fi
        if command -v sha256sum >/dev/null 2>&1; then
            printf "binary-sha256: "
            sha256sum "$binary" | cut -d " " -f 1
        fi
        echo "binary-help-probe:"
        set +e
        "$binary" --help 2>&1
        echo "binary-help-exit: $?"
        set -e
    else
        echo "binary-present: NO"
    fi
    echo
    printf "qtfb-socket: "
    if [ -S /tmp/qtfb.sock ]; then
        echo present
    else
        echo missing
    fi
    echo
    echo "calendar-notes-processes:"
    ps 2>&1 | grep -E '[r]emarkable-calendar-notes|[A]ppLoad' || true
    echo
    echo "all-discovered-app-logs:"
    find /home/root /tmp -type f \
        \( -name calendar-notes.log -o -name calendar-notes.previous.log \) \
        -print 2>/dev/null || true
    echo
    echo "appload-installation:"
    vellum list 2>&1 | grep -i appload || true
    find /home/root/xovi -maxdepth 4 -iname '*appload*' -print 2>/dev/null || true
} >"$work/device-info.txt"

journalctl -u xochitl --since "-30 minutes" --no-pager 2>&1 |
    grep -Ei 'appload|qtfb|remarkable-calendar-notes|external::remarkable-calendar-notes|failed to start|process error' \
    >"$work/appload-xochitl.log" || true

if [ "$include_system_log" = "1" ]; then
    journalctl -u xochitl --since "-10 minutes" --no-pager \
        >"$work/xochitl-last-10-minutes.log" 2>&1 || true
fi

cat >"$work/collection-info.txt" <<EOF
Collector ran on the reMarkable through one SSH session.
App log found: $([ -n "$log_path" ] && echo yes || echo no)
Full xochitl log included: $include_system_log

Calendar Notes excludes calendar contents, source URLs, credentials, and
tokens from its own log. Review optional xochitl logs before sharing.
EOF

if [ "$output_encoding" = "base64" ]; then
    tar -czf - -C "$work" . | base64
else
    tar -czf - -C "$work" .
fi
