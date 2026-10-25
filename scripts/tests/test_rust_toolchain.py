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
        for key in ["APPLE_PICKUP_CARGO", "RUSTC", "CARGO_BUILD_RUSTC", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "RUSTUP_AUTO_INSTALL", "CMAKE"]:
            self.env.pop(key, None)
        self.env.update(PATH="/usr/bin:/bin", FAKE_SYSROOT=str(self.sysroot), FAKE_LOG=str(self.log))
        self.make_tools(self.bin)
        self.cmake = self.root / ".tools/cmake/CMake.app/Contents/bin/cmake"
        self.make_cmake(self.cmake)

    def tearDown(self):
        self.temp.cleanup()

    def executable(self, path, source):
        path.write_text("#!" + sys.executable + "\n" + source)
        path.chmod(0o755)

    def make_tools(self, directory):
        directory.mkdir(parents=True, exist_ok=True)
        self.executable(directory / "cargo", """import json,os,sys
with open(os.environ['FAKE_LOG'],'a') as f:
 f.write(json.dumps({'tool':'cargo','argv':sys.argv[1:],'rustc':os.environ.get('RUSTC'),'cargo_home':os.environ.get('CARGO_HOME'),'rustup_home':os.environ.get('RUSTUP_HOME'),'rustup_toolchain':os.environ.get('RUSTUP_TOOLCHAIN'),'rustup_auto_install':os.environ.get('RUSTUP_AUTO_INSTALL'),'cmake':os.environ.get('CMAKE'),'path':os.environ['PATH']})+'\\n')
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

    def make_cmake(self, path, version="4.4.4"):
        path.parent.mkdir(parents=True, exist_ok=True)
        self.executable(path, "print('cmake version " + version + "')\n")

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
        self.assertEqual(call["cmake"], str(self.cmake))
        self.assertIn(str(self.bin), call["path"].split(":"))
        self.assertIn(str(self.cmake.parent), call["path"].split(":"))

    def test_local_tools_precede_path_but_explicit_overrides_win(self):
        other = self.root / "other bin"
        self.make_tools(other)
        self.make_cmake(other / "cmake")
        result = self.run_script("cargo.sh", "--version", PATH=str(other) + ":/usr/bin:/bin")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls()[-1]["rustc"], str(self.bin / "rustc"))
        self.assertEqual(self.calls()[-1]["cmake"], str(self.cmake))
        result = self.run_script("cargo.sh", "--version", APPLE_PICKUP_CARGO=str(other / "cargo"), CMAKE=str(other / "cmake"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls()[-1]["rustc"], str(other / "rustc"))
        self.assertEqual(self.calls()[-1]["cmake"], str(other / "cmake"))
        result = self.run_script("cargo.sh", "--version", APPLE_PICKUP_CARGO=str(self.bin / "cargo"), RUSTC=str(other / "rustc"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls()[-1]["rustc"], str(other / "rustc"))

    def test_local_tools_ignore_unrelated_cargo_and_rustup_homes(self):
        result = self.run_script("cargo.sh", "--version", CARGO_HOME=str(self.root / "foreign cargo"), RUSTUP_HOME=str(self.root / "foreign rustup"),
                                 RUSTUP_AUTO_INSTALL="1", RUSTUP_TOOLCHAIN="explicit-toolchain")
        self.assertEqual(result.returncode, 0, result.stderr)
        call = self.calls()[-1]
        self.assertEqual(call["cargo_home"], str(self.root / ".tools/cargo"))
        self.assertEqual(call["rustup_home"], str(self.root / ".tools/rustup"))
        self.assertEqual(call["rustup_auto_install"], "0")
        self.assertEqual(call["rustup_toolchain"], "explicit-toolchain")
        self.assertFalse((self.root / "foreign cargo").exists())
        self.assertFalse((self.root / "foreign rustup").exists())

    def test_path_tools_are_supported_without_local_installation(self):
        other = self.root / "external bin"
        self.make_tools(other)
        self.make_cmake(other / "cmake")
        shutil.rmtree(self.bin)
        shutil.rmtree(self.root / ".tools/cmake")
        result = self.run_script("cargo.sh", "--version", PATH=str(other) + ":/usr/bin:/bin",
                                 CARGO_HOME=str(self.root / "external cargo"), RUSTUP_HOME=str(self.root / "external rustup"))
        self.assertEqual(result.returncode, 0, result.stderr)
        call = self.calls()[-1]
        self.assertEqual(call["rustc"], str(other / "rustc"))
        self.assertEqual(call["cmake"], str(other / "cmake"))
        self.assertEqual(call["cargo_home"], str(self.root / "external cargo"))
        self.assertEqual(call["rustup_home"], str(self.root / "external rustup"))

    def test_cargo_home_is_found_without_shell_path_configuration(self):
        external_home = self.root / "external cargo"
        self.make_tools(external_home / "bin")
        shutil.rmtree(self.bin)
        result = self.run_script("cargo.sh", "--version", CARGO_HOME=str(external_home))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls()[-1]["rustc"], str(external_home / "bin/rustc"))

    def test_missing_or_old_cmake_stops_before_cargo(self):
        missing = self.root / "missing cmake"
        result = self.run_script("cargo.sh", "build", CMAKE=str(missing))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("setup-build-tools.sh", result.stderr)
        self.assertFalse(self.calls())
        old = self.root / "old cmake"
        self.make_cmake(old, "3.21.9")
        result = self.run_script("cargo.sh", "build", CMAKE=str(old))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("3.22 or newer", result.stderr)
        self.assertFalse(self.calls())

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
