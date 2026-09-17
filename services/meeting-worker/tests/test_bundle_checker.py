"""Synthetic Mach-O records exercise the clean-Mac checker without a GPU."""

import importlib.util
import hashlib
import json
from pathlib import Path
import subprocess
import struct
import sys
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


    def test_system_path_traversal_is_not_accepted_as_a_system_dependency(self):
        (self.root / "python/bin").mkdir(parents=True)
        executable = self.root / "python/bin/python3.12"
        executable.write_bytes(thin())
        path = self.root / "native.dylib"
        for dependency in ("/usr/lib/../../opt/homebrew/lib/unsafe.dylib",
                           "/System/Library/../../Users/developer/native.dylib"):
            path.write_bytes(thin(dependency=dependency))
            with self.assertRaises(ValueError):
                checker.check_native_dependencies(self.root)

    def test_exact_loader_and_system_rpaths_are_portable_but_homebrew_is_not(self):
        (self.root / "python/bin").mkdir(parents=True)
        executable = self.root / "python/bin/python3.12"
        executable.write_bytes(thin())
        path = self.root / "native.dylib"
        for rpath in ("/usr/lib", "/System/Library", "@loader_path", "@executable_path",
                      "/opt/homebrew/Cellar/gcc@13/13.4.0/lib/gcc/13"):
            data = thin()
            name = rpath.encode() + b"\0"
            length = (12 + len(name) + 7) // 8 * 8
            command = struct.pack("<3I", 0x8000001C, length, 12) + name
            command += b"\0" * (length - len(command))
            header = list(struct.unpack("<8I", data[:32]))
            header[4] += 1; header[5] += len(command)
            path.write_bytes(struct.pack("<8I", *header) + data[32:] + command)
            if rpath.startswith("/opt"):
                with self.assertRaises(ValueError): checker.check_native_dependencies(self.root)
            else:
                checker.check_native_dependencies(self.root)


class AnalysisBundleContractTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.directory = self.root / checker.PROMPT_DIRECTORY
        self.directory.mkdir(parents=True)
        prompts, source_hashes = {}, {}
        source = Path(__file__).resolve().parents[1] / 'src/forum_meeting_worker/prompts/v1'
        for kind in checker.ANALYSIS_TASK_TYPES:
            data = (source / f'{kind}.txt').read_bytes()
            (self.directory / f'{kind}.txt').write_bytes(data)
            sha = hashlib.sha256(data).hexdigest()
            prompts[kind] = {'prompt_version': f'{kind}-v1', 'relative_path': str(checker.PROMPT_DIRECTORY / f'{kind}.txt'),
                             'sha256': sha, 'size_bytes': len(data)}
            source_hashes[f'src/forum_meeting_worker/prompts/v1/{kind}.txt'] = sha
        self.manifest = {'owner': checker.OWNER, 'analysis_task_types': checker.ANALYSIS_TASK_TYPES,
                         'formal_product_acceptance': 'not_evaluated', 'analysis_prompts': prompts,
                         'source_sha256': source_hashes}
        self.save()

    def save(self):
        (self.root / 'runtime-manifest.json').write_text(json.dumps(self.manifest))

    def test_six_prompt_resources_are_hashed_without_claiming_product_acceptance(self):
        actual = checker.check_paths(self.root)
        self.assertEqual(actual['analysis_task_types'], checker.ANALYSIS_TASK_TYPES)
        self.assertEqual(actual['formal_product_acceptance'], 'not_evaluated')
        self.assertNotIn('formal_job_capabilities', actual)

    def test_stale_f01_capabilities_or_product_pass_claim_are_rejected(self):
        self.manifest['formal_job_capabilities'] = []
        self.save()
        with self.assertRaises(ValueError):
            checker.check_paths(self.root)
        self.manifest.pop('formal_job_capabilities')
        self.manifest['formal_product_acceptance'] = 'passed'
        self.save()
        with self.assertRaises(ValueError):
            checker.check_paths(self.root)

    def test_missing_changed_extra_or_symlink_prompt_is_rejected(self):
        target = self.directory / 'minutes.txt'
        original = target.read_bytes()
        target.write_bytes(original + b'changed')
        with self.assertRaisesRegex(ValueError, 'hash/size/source'):
            checker.check_paths(self.root)
        target.unlink()
        with self.assertRaises(ValueError):
            checker.check_paths(self.root)
        target.symlink_to(self.directory / 'insight.txt')
        with self.assertRaises(ValueError):
            checker.check_paths(self.root)
        target.unlink(); target.write_bytes(original)
        (self.directory / 'unexpected.txt').write_text('extra prompt')
        with self.assertRaises(ValueError):
            checker.check_paths(self.root)

    def test_path_traversal_and_source_digest_mismatch_are_rejected(self):
        self.manifest['analysis_prompts']['minutes']['relative_path'] = '../outside.txt'
        self.save()
        with self.assertRaises(ValueError):
            checker.check_paths(self.root)
        self.manifest['analysis_prompts']['minutes']['relative_path'] = str(checker.PROMPT_DIRECTORY / 'minutes.txt')
        self.manifest['source_sha256']['src/forum_meeting_worker/prompts/v1/minutes.txt'] = '0' * 64
        self.save()
        with self.assertRaises(ValueError):
            checker.check_paths(self.root)

    def test_actual_worker_rejects_bad_and_ungranted_jobs_without_loading(self):
        requests = checker.lifecycle_requests(self.root, self.manifest)
        requests.append(checker.rpc('shutdown', 'shutdown'))
        source = str(Path(__file__).resolve().parents[1] / 'src')
        # Isolated interpreter + trusted source path, no package installation,
        # no granted model, no CPU/GPU model or imported legacy application.
        code = f'import sys; sys.path.insert(0,{source!r}); from forum_meeting_worker.main import main; raise SystemExit(main())'
        before = sorted(str(p.relative_to(self.root)) for p in self.root.rglob('*'))
        result = subprocess.run([sys.executable, '-I', '-B', '-c', code],
            input=''.join(json.dumps(r) + '\n' for r in requests), text=True,
            capture_output=True, timeout=5, env=checker.isolated_environment(self.root), cwd=self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        replies = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(checker.validate_lifecycle_replies(requests, replies),
                         {'malformed': 'INVALID_PARAMS', 'ungranted': 'MODEL_UNAVAILABLE'})
        self.assertEqual(before, sorted(str(p.relative_to(self.root)) for p in self.root.rglob('*')))
        stale = json.loads(json.dumps(replies))
        stale[0]['result']['capabilities']['task_types'] = []
        with self.assertRaises(ValueError):
            checker.validate_lifecycle_replies(requests, stale)
        wrong_rejection = json.loads(json.dumps(replies))
        wrong_rejection[3] = {'jsonrpc': '2.0', 'id': 'ungranted', 'result': {'status': 'succeeded'}}
        with self.assertRaises(ValueError):
            checker.validate_lifecycle_replies(requests, wrong_rejection)



if __name__ == "__main__":
    unittest.main()
