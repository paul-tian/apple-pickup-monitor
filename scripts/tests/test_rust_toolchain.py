#!/usr/bin/env python3
"""Exercise build/setup selection with fake compilers; never download toolchains."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parents[1]


class ToolchainScripts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="pickup rust targets ")
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        for name in ["cargo.sh", "rust-toolchain.sh", "setup-rust-targets.sh", "build-helper.sh"]:
            shutil.copy2(SOURCE / name, self.root / "scripts" / name)
        self.bin = self.root / ".tools/cargo/bin"
        self.bin.mkdir(parents=True)
        self.sysroot = self.root / ".tools/rustup/toolchains/test-aarch64-apple-darwin"
        self.sysroot.mkdir(parents=True)
        self.log = self.root / "calls.jsonl"
        self.env = os.environ.copy()
        for key in ["APPLE_PICKUP_CARGO", "RUSTC", "CARGO_BUILD_RUSTC", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN"]:
            self.env.pop(key, None)
        self.env.update(PATH="/usr/bin:/bin", FAKE_SYSROOT=str(self.sysroot), FAKE_LOG=str(self.log))
        self.make_tools(self.bin)

    def tearDown(self):
        self.temp.cleanup()

    def executable(self, path, source):
        path.write_text("#!" + sys.executable + "\n" + source)
        path.chmod(0o755)

    def make_tools(self, directory):
        directory.mkdir(parents=True, exist_ok=True)
        self.executable(directory / "cargo", """import json,os,sys
with open(os.environ['FAKE_LOG'],'a') as f:
 f.write(json.dumps({'tool':'cargo','argv':sys.argv[1:],'rustc':os.environ.get('RUSTC'),'cargo_home':os.environ.get('CARGO_HOME'),'rustup_home':os.environ.get('RUSTUP_HOME')})+'\\n')
print('fake cargo')
""")
        self.executable(directory / "rustc", """import os,pathlib,sys
root=pathlib.Path(os.environ['FAKE_SYSROOT'])
if sys.argv[1:]==['--print','sysroot']: print(root)
elif sys.argv[1:3]==['--print','target-libdir']: print(root/'lib/rustlib'/sys.argv[-1]/'lib')
else: sys.exit(2)
""")
        self.executable(directory / "rustup", """import json,os,pathlib,sys
with open(os.environ['FAKE_LOG'],'a') as f:
 f.write(json.dumps({'tool':'rustup','argv':sys.argv[1:],'rustup_home':os.environ.get('RUSTUP_HOME')})+'\\n')
if sys.argv[1]=='run': print(os.environ.get('FAKE_MANAGED_SYSROOT',os.environ['FAKE_SYSROOT']))
elif sys.argv[1:3]==['target','add']:
 name=sys.argv[sys.argv.index('--toolchain')+1]
 for target in sys.argv[sys.argv.index('--toolchain')+2:]:
  lib=pathlib.Path(os.environ['RUSTUP_HOME'])/'toolchains'/name/'lib/rustlib'/target/'lib'
  lib.mkdir(parents=True,exist_ok=True)
  (lib/'libcore-test.rlib').touch();(lib/'libstd-test.rlib').touch()
else: sys.exit(2)
""")

    def run_script(self, name, *arguments, **environment):
        env = dict(self.env, **environment)
        return subprocess.run(["/bin/bash", str(self.root / "scripts" / name), *arguments], cwd=self.root, env=env, text=True, capture_output=True)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def installed(self, target):
        lib = self.sysroot / "lib/rustlib" / target / "lib"
        lib.mkdir(parents=True)
        (lib / "libcore-test.rlib").touch()
        (lib / "libstd-test.rlib").touch()

    def test_project_local_selection_and_paths_with_spaces(self):
        result = self.run_script("cargo.sh", "--version")
        self.assertEqual(result.returncode, 0, result.stderr)
        call = self.calls()[0]
        self.assertEqual(call["rustc"], str(self.bin / "rustc"))
        self.assertEqual(call["cargo_home"], str(self.root / ".tools/cargo"))
        self.assertEqual(call["rustup_home"], str(self.root / ".tools/rustup"))

    def test_path_and_explicit_overrides_take_precedence(self):
        other = self.root / "other bin"
        self.make_tools(other)
        result = self.run_script("cargo.sh", "--version", PATH=str(other) + ":/usr/bin:/bin")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls()[-1]["rustc"], str(other / "rustc"))
        result = self.run_script("cargo.sh", "--version", APPLE_PICKUP_CARGO=str(self.bin / "cargo"), RUSTC=str(other / "rustc"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls()[-1]["rustc"], str(other / "rustc"))

    def test_all_architectures_are_checked_before_any_compile(self):
        self.installed("aarch64-apple-darwin")
        result = self.run_script("build-helper.sh", APPLE_PICKUP_CARGO=str(self.bin / "cargo"),
            ARCHS="arm64 x86_64", ONLY_ACTIVE_ARCH="NO", TARGET_BUILD_DIR=str(self.root / "build"), CONTENTS_FOLDER_PATH="Test.app/Contents")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("x86_64-apple-darwin", result.stderr)
        self.assertIn("setup-rust-targets.sh", result.stderr)
        self.assertFalse(self.calls(), "Cargo started compiling before all targets were checked")

    def test_setup_installs_into_selected_compiler_not_default_home(self):
        result = self.run_script("setup-rust-targets.sh", APPLE_PICKUP_CARGO=str(self.bin / "cargo"), RUSTUP_HOME=str(self.root / "unrelated"))
        self.assertEqual(result.returncode, 0, result.stderr)
        install = self.calls()[-1]
        self.assertEqual(install["argv"], ["target", "add", "--toolchain", self.sysroot.name, "aarch64-apple-darwin", "x86_64-apple-darwin"])
        self.assertEqual(install["rustup_home"], str(self.root / ".tools/rustup"))

    def test_setup_refuses_mismatched_or_unmanaged_compilers(self):
        result = self.run_script("setup-rust-targets.sh", APPLE_PICKUP_CARGO=str(self.bin / "cargo"), FAKE_MANAGED_SYSROOT=str(self.root / "wrong"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Refusing", result.stderr)
        self.assertFalse(any(call["argv"][:2] == ["target", "add"] for call in self.calls()))
        result = self.run_script("setup-rust-targets.sh", APPLE_PICKUP_CARGO=str(self.bin / "cargo"), FAKE_SYSROOT=str(self.root / "non-rustup"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("compiler distribution", result.stderr)

    def test_setup_rejects_unrelated_targets(self):
        result = self.run_script("setup-rust-targets.sh", "wasm32-unknown-unknown", APPLE_PICKUP_CARGO=str(self.bin / "cargo"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Unsupported macOS Rust target", result.stderr)
        self.assertFalse(self.calls())


if __name__ == "__main__":
    unittest.main()
