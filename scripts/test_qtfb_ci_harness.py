import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("qtfb_ci_harness.py")
SPEC = importlib.util.spec_from_file_location("qtfb_ci_harness", SCRIPT)
assert SPEC and SPEC.loader
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)


class QtfbHarnessTests(unittest.TestCase):
    def test_initialize_round_trip_uses_arm32_reply_layout(self):
        packet = bytearray(HARNESS.MESSAGE_LEN)
        packet[0] = HARNESS.MESSAGE_INITIALIZE
        packet[4:8] = (12345).to_bytes(4, "little", signed=True)
        packet[8] = HARNESS.FBFMT_RM2FB
        self.assertEqual(HARNESS.parse_initialize(bytes(packet)), (12345, 0))

        reply = HARNESS.initialize_reply(12345)
        self.assertEqual(reply[0], HARNESS.MESSAGE_INITIALIZE)
        self.assertEqual(int.from_bytes(reply[4:8], "little", signed=True), 12345)
        self.assertEqual(
            int.from_bytes(reply[8:12], "little"), HARNESS.FRAME_BYTES
        )

    def test_full_and_partial_updates_are_decoded(self):
        full = bytearray(HARNESS.MESSAGE_LEN)
        full[0] = HARNESS.MESSAGE_UPDATE
        self.assertEqual(HARNESS.parse_update(bytes(full)), (0, None))

        partial = bytearray(HARNESS.MESSAGE_LEN)
        partial[0] = HARNESS.MESSAGE_UPDATE
        partial[4:8] = (1).to_bytes(4, "little", signed=True)
        for offset, value in zip((8, 12, 16, 20), (10, 20, 30, 40)):
            partial[offset : offset + 4] = value.to_bytes(4, "little", signed=True)
        self.assertEqual(
            HARNESS.parse_update(bytes(partial)), (1, (10, 20, 30, 40))
        )

    def test_rgb565_conversion_writes_a_valid_ppm(self):
        ppm = HARNESS.rgb565_to_ppm(b"\x00\xf8\xe0\x07\x1f\x00", 3, 1)
        self.assertEqual(ppm, b"P6\n3 1\n255\n\xff\x00\x00\x00\xff\x00\x00\x00\xff")

    def test_manifest_must_describe_this_qtfb_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "remarkable-calendar-notes"
            binary.touch()
            manifest = root / "external.manifest.json"
            manifest.write_text(
                json.dumps(
                    {
                        "application": binary.name,
                        "qtfb": True,
                        "aspectRatio": "original",
                    }
                ),
                encoding="utf-8",
            )
            self.assertTrue(HARNESS.validate_manifest(manifest, binary)["qtfb"])

            manifest.write_text(
                json.dumps(
                    {
                        "application": binary.name,
                        "qtfb": False,
                        "aspectRatio": "original",
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaises(ValueError):
                HARNESS.validate_manifest(manifest, binary)

    def test_launch_command_supports_an_arm_emulator_and_sysroot(self):
        binary = Path("/tmp/remarkable-calendar-notes")
        emulator = Path("/usr/bin/qemu-arm")
        sysroot = Path("/usr/arm-linux-gnueabihf")
        self.assertEqual(
            HARNESS.launch_command(binary, emulator, sysroot),
            [
                str(emulator),
                "-L",
                str(sysroot),
                str(binary),
                "run",
            ],
        )
        with self.assertRaises(ValueError):
            HARNESS.launch_command(binary, sysroot=Path("/tmp/sysroot"))


if __name__ == "__main__":
    unittest.main()
