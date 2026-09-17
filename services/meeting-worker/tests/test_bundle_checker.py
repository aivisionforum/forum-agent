"""Synthetic Mach-O records exercise the clean-Mac checker without a GPU."""

import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[3] / "desktop/scripts/check_meeting_worker_bundle.py"
spec = importlib.util.spec_from_file_location("worker_bundle_checker", SCRIPT)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


def thin(*, cpu=0x0100000C, minimum=0x000E0000, dependency="/usr/lib/libSystem.B.dylib"):
    name = dependency.encode() + b"\0"
    length = (24 + len(name) + 7) // 8 * 8
    load = struct.pack("<6I", 0xC, length, 24, 0, 0, 0) + name
    load += b"\0" * (length - len(load))
    os_version = struct.pack("<6I", 0x32, 24, 1, minimum, minimum, 0)
    # This absolute install ID must not be mistaken for a runtime dependency.
    install_id = load.replace(b"/usr/lib/", b"/install/", 1)
    install_id = struct.pack("<I", 0xD) + install_id[4:]
    commands = load + os_version + install_id
    return struct.pack("<8I", 0xFEEDFACF, cpu, 0, 6, 3, len(commands), 0, 0) + commands


class MachOTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_thin_arm64_loads_and_id_are_distinguished(self):
        path = self.root / "native.dylib"
        path.write_bytes(thin())
        result = checker.macho_arm64(path)
        self.assertEqual(result["minimum_macos"], (14, 0, 0))
        self.assertEqual(result["dependencies"], ["/usr/lib/libSystem.B.dylib"])

    def test_fat_selects_arm64_and_rejects_out_of_bounds_or_x86_only(self):
        path = self.root / "fat.dylib"
        arm, x86 = thin(), thin(cpu=0x01000007)
        header = struct.pack(">II", 0xCAFEBABE, 2)
        header += struct.pack(">5I", 0x01000007, 3, 48, len(x86), 0)
        header += struct.pack(">5I", 0x0100000C, 0, 48 + len(x86), len(arm), 0)
        path.write_bytes(header + x86 + arm)
        self.assertEqual(checker.macho_arm64(path)["architecture"], "arm64")
        for data in (thin(cpu=0x01000007), header + x86, b"not a native file"):
            path.write_bytes(data)
            with self.assertRaises(ValueError):
                checker.macho_arm64(path)

    def test_truncated_commands_and_developer_library_paths_are_rejected(self):
        path = self.root / "native.dylib"
        path.write_bytes(thin()[:-1])
        with self.assertRaises(ValueError):
            checker.macho_arm64(path)
        (self.root / "python/bin").mkdir(parents=True)
        executable = self.root / "python/bin/python3.12"
        executable.write_bytes(thin())
        for data in (thin(dependency="/opt/homebrew/lib/not-portable.dylib"), thin(minimum=0x001A0000)):
            path.write_bytes(data)
            with self.assertRaises(ValueError):
                checker.check_native_dependencies(self.root)


if __name__ == "__main__":
    unittest.main()
