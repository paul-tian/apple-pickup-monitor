#!/usr/bin/env python3
"""Run bootstrap behavior against local download fixtures; never install real tools."""
import hashlib
import io
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tarfile
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]
RUST_VERSION = "1.99.1"
RUSTUP_VERSION = "1.29.2"
CMAKE_VERSION = "4.4.4"

RUSTUP = r'''
import json, os, pathlib, sys
args = sys.argv[1:]
root = pathlib.Path(os.environ['RUSTUP_HOME'])
with open(os.environ['FAKE_LOG'], 'a') as log:
    log.write(json.dumps({'tool':'rustup', 'args':args, 'cargo':os.environ['CARGO_HOME'],
        'rustup':str(root), 'override':os.environ.get('RUSTUP_TOOLCHAIN'),
        'server':os.environ['RUSTUP_DIST_SERVER']}) + '\n')
def target(tc, name):
    lib = root/'toolchains'/tc/'lib/rustlib'/name/'lib'
    lib.mkdir(parents=True, exist_ok=True)
    for kind in ['core', 'std']:
        (lib/('lib'+kind+'-fixture.rlib')).touch()
def component_path(tc):
    return root/'toolchains'/tc/'components.json'
if args == ['--version']:
    print('rustup 1.29.2 (fixture)')
elif args[:2] == ['set', 'auto-self-update']:
    root.mkdir(parents=True, exist_ok=True)
elif args[0] == 'run':
    tc = args[1]
    if not (root/'toolchains'/tc/'installed').exists(): sys.exit(1)
    if args[3:] == ['--version']: print('rustc 1.99.1 (fixture)')
    elif args[3:5] == ['--print', 'target-libdir']:
        print(root/'toolchains'/tc/'lib/rustlib'/args[-1]/'lib')
    else: sys.exit('Unexpected fake rustc arguments: '+repr(args))
elif args[:2] == ['toolchain', 'install']:
    tc = args[2]
    base = root/'toolchains'/tc
    base.mkdir(parents=True, exist_ok=True)
    (base/'installed').touch()
    components = []
    for index, arg in enumerate(args):
        if arg == '--target': target(tc, args[index+1])
        if arg == '--component': components.append(args[index+1])
    component_path(tc).write_text(json.dumps(components))
elif args[:2] == ['target', 'add']:
    target(args[args.index('--toolchain')+1], args[-1])
elif args[:2] == ['component', 'list']:
    tc = args[args.index('--toolchain')+1]
    for name in json.loads(component_path(tc).read_text()):
        print(name+'-'+tc.split('-',1)[1])
elif args[:2] == ['component', 'add']:
    tc = args[args.index('--toolchain')+1]
    path = component_path(tc)
    path.write_text(json.dumps(json.loads(path.read_text())+[args[-1]]))
elif args[0] == 'default':
    (root/'default').write_text(args[1])
else:
    sys.exit('Unexpected fake rustup arguments: '+repr(args))
'''


class BuildToolsSetup(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="pickup setup with spaces ")
        self.root = Path(self.temp.name)
        self.scripts = self.root / "scripts"
        self.scripts.mkdir()
        self.fixtures = self.root / "fixtures"
        self.fixtures.mkdir()
        self.log = self.root / "calls.jsonl"
        self.env = dict(os.environ, FAKE_LOG=str(self.log), FAKE_FIXTURES=str(self.fixtures),
                        CARGO_HOME=str(self.root / "unrelated cargo"),
                        RUSTUP_HOME=str(self.root / "unrelated rustup"),
                        RUSTUP_TOOLCHAIN="unrelated-toolchain",
                        RUSTUP_DIST_SERVER="https://unrelated.invalid",
                        PATH="/usr/bin:/bin:/usr/sbin:/sbin")
        self.write_executable(self.fixtures / "rustup", RUSTUP)
        installer = self.write_executable(self.fixtures / "rustup-init", r'''
import json, os, pathlib, shutil, sys
with open(os.environ['FAKE_LOG'], 'a') as log:
    log.write(json.dumps({'tool':'installer','args':sys.argv[1:],
        'cargo':os.environ['CARGO_HOME'],'rustup':os.environ['RUSTUP_HOME']})+'\n')
destination = pathlib.Path(os.environ['CARGO_HOME'])/'bin'
destination.mkdir(parents=True, exist_ok=True)
shutil.copy2(pathlib.Path(os.environ['FAKE_FIXTURES'])/'rustup', destination/'rustup')
''')
        cmake = self.executable_bytes("import os\nprint('cmake version '+('0.0.0' if os.environ.get('FAKE_BROKEN_CMAKE') else '4.4.4'))\n")
        archive = self.fixtures / "cmake.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            entry = tarfile.TarInfo("cmake-fixture/CMake.app/Contents/bin/cmake")
            entry.mode = 0o755
            entry.size = len(cmake)
            tar.addfile(entry, io.BytesIO(cmake))
        rust_hash = hashlib.sha256(installer.read_bytes()).hexdigest()
        cmake_hash = hashlib.sha256(archive.read_bytes()).hexdigest()
        (self.scripts / "build-tools.env").write_text(
            f"PICKUP_RUST_VERSION={RUST_VERSION}\nPICKUP_RUSTUP_VERSION={RUSTUP_VERSION}\n"
            f"PICKUP_RUSTUP_ARM64_SHA256={rust_hash}\nPICKUP_RUSTUP_X86_64_SHA256={rust_hash}\n"
            f"PICKUP_CMAKE_VERSION={CMAKE_VERSION}\nPICKUP_CMAKE_SHA256={cmake_hash}\n")
        fakebin = self.root / "fake bin"
        fakebin.mkdir()
        self.write_executable(fakebin / "uname", "import os,sys\nprint('Darwin' if sys.argv[1]=='-s' else os.environ.get('FAKE_ARCH','arm64'))\n")
        self.write_executable(fakebin / "xcodebuild", "print('Xcode fixture')\n")
        self.write_executable(fakebin / "xcrun", "print('/fixture/clang')\n")
        self.write_executable(fakebin / "curl", r'''
import json, os, pathlib, shutil, sys
args = sys.argv[1:]
url = args[-1]
with open(os.environ['FAKE_LOG'], 'a') as log:
    log.write(json.dumps({'tool':'curl','args':args})+'\n')
kind = 'rustup-init' if url.endswith('/rustup-init') else 'cmake.tar.gz'
destination = pathlib.Path(args[args.index('--output')+1])
if os.environ.get('FAKE_CORRUPT_DOWNLOAD') == kind:
    destination.write_bytes(b'corrupted fixture download')
else:
    shutil.copyfile(pathlib.Path(os.environ['FAKE_FIXTURES'])/kind, destination)
''')
        source = (SCRIPTS / "setup-build-tools.sh").read_text()
        # Only replace external platform/network commands, not bootstrap logic.
        for command in ["uname", "xcodebuild", "xcrun", "curl"]:
            source = source.replace("/usr/bin/" + command, shlex.quote(str(fakebin / command)))
        self.script = self.scripts / "setup-build-tools.sh"
        self.script.write_text(source)

    def tearDown(self):
        self.temp.cleanup()

    def executable_bytes(self, source):
        return ("#!" + sys.executable + "\n" + source).encode()

    def write_executable(self, path, source):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(self.executable_bytes(source))
        path.chmod(0o755)
        return path

    def run_setup(self, **environment):
        return subprocess.run(["/bin/bash", str(self.script)], cwd=self.root,
                              env=dict(self.env, **environment), capture_output=True, text=True)

    def calls(self, tool):
        entries = [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []
        return [entry for entry in entries if entry["tool"] == tool]

    def assert_success(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((self.root / ".tools/.setup-lock").exists())

    def test_first_setup_is_local_and_installs_both_architectures(self):
        self.assert_success(self.run_setup())
        installer = self.calls("installer")[0]
        self.assertIn("--no-modify-path", installer["args"])
        self.assertEqual(installer["cargo"], str(self.root / ".tools/cargo"))
        self.assertEqual(installer["rustup"], str(self.root / ".tools/rustup"))
        for entry in self.calls("rustup"):
            self.assertEqual(entry["cargo"], str(self.root / ".tools/cargo"))
            self.assertEqual(entry["rustup"], str(self.root / ".tools/rustup"))
            self.assertIsNone(entry["override"])
            self.assertEqual(entry["server"], "https://static.rust-lang.org")
        toolchain = self.root / ".tools/rustup/toolchains/1.99.1-aarch64-apple-darwin"
        for target in ["aarch64-apple-darwin", "x86_64-apple-darwin"]:
            self.assertTrue((toolchain / "lib/rustlib" / target / "lib/libstd-fixture.rlib").exists())
        self.assertFalse((self.root / "unrelated cargo").exists())
        self.assertFalse((self.root / "unrelated rustup").exists())
        self.assertTrue((self.root / ".tools/cmake/CMake.app/Contents/bin/cmake").is_file())

    def test_complete_rerun_does_not_download_or_reinstall(self):
        self.assert_success(self.run_setup())
        downloads = self.calls("curl")
        installations = [entry for entry in self.calls("rustup") if entry["args"][:2] == ["toolchain", "install"]]
        self.assert_success(self.run_setup())
        self.assertEqual(self.calls("curl"), downloads)
        self.assertEqual(len(self.calls("installer")), 1)
        self.assertEqual([entry for entry in self.calls("rustup") if entry["args"][:2] == ["toolchain", "install"]], installations)

    def test_partial_target_is_repaired_without_reinstall(self):
        self.assert_success(self.run_setup())
        missing = self.root / ".tools/rustup/toolchains/1.99.1-aarch64-apple-darwin/lib/rustlib/x86_64-apple-darwin/lib/libstd-fixture.rlib"
        missing.unlink()
        self.assert_success(self.run_setup())
        repairs = [entry for entry in self.calls("rustup") if entry["args"][:2] == ["target", "add"]]
        self.assertEqual(len(repairs), 1)
        self.assertEqual(repairs[0]["args"][-1], "x86_64-apple-darwin")
        self.assertTrue(missing.exists())
        self.assertEqual(len(self.calls("curl")), 2)

    def test_bad_installer_checksum_prevents_execution(self):
        result = self.run_setup(FAKE_CORRUPT_DOWNLOAD="rustup-init")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Checksum verification failed", result.stderr)
        self.assertFalse(self.calls("installer"))
        self.assertFalse((self.root / ".tools/cargo").exists())
        self.assertFalse((self.root / ".tools/.setup-lock").exists())

    def test_invalid_cmake_preserves_existing_installation(self):
        old = self.write_executable(self.root / ".tools/cmake/CMake.app/Contents/bin/cmake", "print('cmake version 3.27.0')\n")
        before = old.read_bytes()
        result = self.run_setup(FAKE_BROKEN_CMAKE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CMake installation failed validation", result.stderr)
        self.assertEqual(old.read_bytes(), before)
        self.assertFalse((self.root / ".tools/.setup-lock").exists())

    def test_broken_existing_cmake_is_replaced(self):
        old = self.write_executable(self.root / ".tools/cmake/CMake.app/Contents/bin/cmake", "import sys\nsys.exit(2)\n")
        before = old.read_bytes()
        self.assert_success(self.run_setup())
        self.assertNotEqual(old.read_bytes(), before)
        result = subprocess.run([str(old), "--version"], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0)
        self.assertIn("cmake version 4.4.4", result.stdout)

    def test_download_cache_symlink_cannot_write_outside_tools(self):
        outside = self.root / "unrelated installation"
        outside.mkdir()
        cached = self.root / ".tools/downloads/rustup-init-1.29.2-aarch64-apple-darwin"
        cached.parent.mkdir(parents=True)
        cached.symlink_to(outside, target_is_directory=True)
        result = self.run_setup()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(list(outside.iterdir()), "cache symlink redirected a write outside .tools")
        self.assertFalse(self.calls("installer"))
        self.assertFalse((self.root / ".tools/.setup-lock").exists())

    def test_intel_host_downloads_intel_rustup(self):
        self.assert_success(self.run_setup(FAKE_ARCH="x86_64"))
        self.assertIn("/x86_64-apple-darwin/rustup-init", self.calls("curl")[0]["args"][-1])
        self.assertEqual((self.root / ".tools/rustup/default").read_text(), "1.99.1-x86_64-apple-darwin")


if __name__ == "__main__":
    unittest.main()
