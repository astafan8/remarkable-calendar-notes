#!/usr/bin/env python3
"""Minimal AppLoad QTFB host for Linux CI.

This launches the real host-compiled application in device mode, implements
the QTFB initialize/update subset it uses, and captures shared RGB565 pixels.
It intentionally does not emulate reMarkable OS, xochitl, XOVI, or e-ink.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import time

WIDTH = 1404
HEIGHT = 1872
FRAME_BYTES = WIDTH * HEIGHT * 2
MESSAGE_LEN = 24
MESSAGE_INITIALIZE = 0
MESSAGE_UPDATE = 1
MESSAGE_TERMINATE = 3
UPDATE_ALL = 0
FBFMT_RM2FB = 0
SOCKET_PATH = Path("/tmp/qtfb.sock")


def parse_initialize(packet: bytes) -> tuple[int, int]:
    if len(packet) != MESSAGE_LEN or packet[0] != MESSAGE_INITIALIZE:
        raise ValueError("expected a 24-byte QTFB initialize packet")
    key = struct.unpack_from("=i", packet, 4)[0]
    framebuffer_format = packet[8]
    if framebuffer_format != FBFMT_RM2FB:
        raise ValueError(f"expected RM2 RGB565 format 0, got {framebuffer_format}")
    return key, framebuffer_format


def initialize_reply(key: int, size: int = FRAME_BYTES) -> bytes:
    packet = bytearray(MESSAGE_LEN)
    packet[0] = MESSAGE_INITIALIZE
    struct.pack_into("=iI", packet, 4, key, size)
    return bytes(packet)


def parse_update(packet: bytes) -> tuple[int, tuple[int, int, int, int] | None]:
    if len(packet) != MESSAGE_LEN or packet[0] != MESSAGE_UPDATE:
        raise ValueError("expected a 24-byte QTFB update packet")
    mode = struct.unpack_from("=i", packet, 4)[0]
    if mode == UPDATE_ALL:
        return mode, None
    return mode, struct.unpack_from("=iiii", packet, 8)


def rgb565_to_ppm(frame: bytes, width: int = WIDTH, height: int = HEIGHT) -> bytes:
    if len(frame) != width * height * 2:
        raise ValueError("RGB565 frame has the wrong size")
    rgb = bytearray(width * height * 3)
    dst = 0
    for low, high in zip(frame[0::2], frame[1::2]):
        pixel = low | (high << 8)
        rgb[dst] = ((pixel >> 11) & 0x1F) * 255 // 31
        rgb[dst + 1] = ((pixel >> 5) & 0x3F) * 255 // 63
        rgb[dst + 2] = (pixel & 0x1F) * 255 // 31
        dst += 3
    return f"P6\n{width} {height}\n255\n".encode() + rgb


def validate_manifest(path: Path, binary: Path) -> dict[str, object]:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if manifest.get("qtfb") is not True:
        raise ValueError("external.manifest.json must enable qtfb")
    application = manifest.get("application")
    args = manifest.get("args") or []
    # The app may be launched directly (application == the binary) or through
    # a shell wrapper (application == a shell, with the binary named in args
    # so the wrapper can fix its execute bit and exec it). Accept either, but
    # require that the manifest actually references this binary somewhere.
    application_names_binary = (
        isinstance(application, str) and Path(application).name == binary.name
    )
    args_reference_binary = any(binary.name in str(arg) for arg in args)
    if not (application_names_binary or args_reference_binary):
        raise ValueError(
            f"manifest neither runs nor references {binary.name!r} "
            f"(application={application!r}, args={args!r})"
        )
    if manifest.get("aspectRatio") != "original":
        raise ValueError("manifest must select the original RM1/RM2 aspect ratio")
    return manifest


def launch_command(
    binary: Path, emulator: Path | None = None, sysroot: Path | None = None
) -> list[str]:
    command = []
    if emulator is not None:
        command.append(str(emulator))
        if sysroot is not None:
            command.extend(["-L", str(sysroot)])
    elif sysroot is not None:
        raise ValueError("--sysroot requires --emulator")
    command.extend([str(binary), "run"])
    return command


def capture(args: argparse.Namespace) -> None:
    if sys.platform != "linux" or not hasattr(socket, "SOCK_SEQPACKET"):
        raise RuntimeError("the QTFB harness requires Linux SOCK_SEQPACKET support")

    binary = args.binary.resolve()
    manifest = args.manifest.resolve()
    screenshot = args.screenshot.resolve()
    data_dir = args.data_dir.resolve()
    log_path = args.process_log.resolve()
    emulator = args.emulator.resolve() if args.emulator is not None else None
    sysroot = args.sysroot.resolve() if args.sysroot is not None else None
    if not binary.is_file():
        raise FileNotFoundError(binary)
    if emulator is not None and not emulator.is_file():
        raise FileNotFoundError(emulator)
    if sysroot is not None and not sysroot.is_dir():
        raise FileNotFoundError(sysroot)
    validate_manifest(manifest, binary)

    screenshot.parent.mkdir(parents=True, exist_ok=True)
    data_dir.mkdir(parents=True, exist_ok=True)
    log_path.parent.mkdir(parents=True, exist_ok=True)

    key = 100_000 + (os.getpid() % 1_000_000)
    shm_path = Path(f"/dev/shm/qtfb_{key}")
    server = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    process: subprocess.Popen[bytes] | None = None
    connection: socket.socket | None = None
    output_captured = False
    started = time.monotonic()
    full_updates = 0

    try:
        if SOCKET_PATH.exists():
            raise RuntimeError(f"{SOCKET_PATH} already exists; refusing to replace it")
        server.bind(str(SOCKET_PATH))
        server.listen(1)
        server.settimeout(args.timeout)

        environment = os.environ.copy()
        environment.update(
            {
                "QTFB_KEY": str(key),
                "REMARKABLE_CALENDAR_NOTES_DATA_DIR": str(data_dir),
            }
        )
        process = subprocess.Popen(
            launch_command(binary, emulator, sysroot),
            cwd=str(manifest.parent),
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )

        connection, _ = server.accept()
        connection.settimeout(args.timeout)
        requested_key, _ = parse_initialize(connection.recv(MESSAGE_LEN))
        if requested_key != key:
            raise RuntimeError(
                f"client requested framebuffer key {requested_key}, expected {key}"
            )

        with shm_path.open("w+b") as shared:
            shared.truncate(FRAME_BYTES)
            shared.write(b"\xff" * FRAME_BYTES)
            shared.flush()
            connection.sendall(initialize_reply(key))

            while time.monotonic() - started < args.timeout:
                packet = connection.recv(MESSAGE_LEN)
                if not packet:
                    raise RuntimeError("application closed QTFB before a full update")
                if packet[0] == MESSAGE_TERMINATE:
                    raise RuntimeError("application terminated before a full update")
                if packet[0] != MESSAGE_UPDATE:
                    continue
                mode, _ = parse_update(packet)
                if mode != UPDATE_ALL:
                    continue
                full_updates += 1
                if full_updates <= args.drop_full_updates:
                    print(
                        f"dropped full update {full_updates}/"
                        f"{args.drop_full_updates} to simulate AppLoad attachment"
                    )
                    continue
                shared.seek(0)
                frame = shared.read(FRAME_BYTES)
                non_white = sum(
                    1
                    for index in range(0, len(frame), 2)
                    if frame[index] != 0xFF or frame[index + 1] != 0xFF
                )
                if non_white == 0:
                    raise RuntimeError("application published a blank framebuffer")
                screenshot.write_bytes(rgb565_to_ppm(frame))
                print(
                    f"captured {screenshot} ({WIDTH}x{HEIGHT}, "
                    f"{non_white} non-white pixels)"
                )
                # Keep the host connected long enough for all fixed startup
                # repaints to complete. Closing immediately after capture can
                # race the next retry and turn an otherwise successful test
                # into an artificial broken-pipe failure.
                time.sleep(args.post_capture_delay)
                break
            else:
                raise TimeoutError("timed out waiting for a full QTFB update")

        connection.close()
        connection = None
        output, _ = process.communicate(timeout=5)
        log_path.write_bytes(output)
        output_captured = True
        if process.returncode != 0:
            raise RuntimeError(
                f"application exited with {process.returncode}; see {log_path}"
            )
    finally:
        if connection is not None:
            connection.close()
        server.close()
        if process is not None and not output_captured:
            if process.poll() is None:
                process.terminate()
            try:
                output, _ = process.communicate(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                output, _ = process.communicate()
            log_path.write_bytes(output)
        SOCKET_PATH.unlink(missing_ok=True)
        shm_path.unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument(
        "--manifest", type=Path, default=Path("external.manifest.json")
    )
    parser.add_argument("--screenshot", type=Path, required=True)
    parser.add_argument(
        "--data-dir", type=Path, default=Path("target/qtfb-ci-data")
    )
    parser.add_argument(
        "--process-log", type=Path, default=Path("target/qtfb-ci-process.log")
    )
    parser.add_argument(
        "--emulator",
        type=Path,
        help="Optional executable such as qemu-arm used to launch the binary",
    )
    parser.add_argument(
        "--sysroot",
        type=Path,
        help="Optional emulator dynamic-loader root (passed as -L)",
    )
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument(
        "--drop-full-updates",
        type=int,
        default=0,
        help="Ignore this many initial full updates to exercise startup recovery",
    )
    parser.add_argument(
        "--post-capture-delay",
        type=float,
        default=2.0,
        help="Seconds to keep QTFB connected after capturing the frame",
    )
    args = parser.parse_args()
    if args.drop_full_updates < 0:
        parser.error("--drop-full-updates must be non-negative")
    if args.post_capture_delay < 0:
        parser.error("--post-capture-delay must be non-negative")
    try:
        capture(args)
    except (OSError, RuntimeError, TimeoutError, ValueError) as error:
        print(f"qtfb-ci-harness: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
