#!/usr/bin/env python3
"""Exercise packaging metadata with a fake Xcode build; no signing or network."""
import json
import os
from pathlib import Path
import plistlib
import shlex
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]


class AppVersionPackaging(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="pickup version with spaces ")
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        self.bin = self.root / "fake bin"
        self.bin.mkdir()
        self.log = self.root / "calls.jsonl"
        self.env = os.environ.copy()
        for key in ["APP_VERSION", "BUILD_NUMBER", "APPLE_PICKUP_TARGET", "SIGNING_IDENTITY"]:
            self.env.pop(key, None)
        self.env.update(FAKE_LOG=str(self.log), PATH=str(self.bin) + ":/usr/bin:/bin",
                        FAKE_HOST_ARCH="arm64")
        self.tool("uname", "import os,sys\nprint('Darwin' if sys.argv[1]=='-s' else os.environ['FAKE_HOST_ARCH'])\n")
        self.tool("xcodebuild", r'''
import json, os, pathlib, plistlib, sys
args = sys.argv[1:]
with open(os.environ['FAKE_LOG'], 'a') as log:
    log.write(json.dumps({'tool':'xcodebuild','args':args})+'\n')
settings = dict(arg.split('=', 1) for arg in args if '=' in arg)
version = settings.get('MARKETING_VERSION', '0.5.0')
build = settings.get('CURRENT_PROJECT_VERSION', '42')
app = pathlib.Path(args[args.index('-derivedDataPath')+1])/'Build/Products/Release/Apple Pickup Monitor.app'
(app/'Contents/MacOS').mkdir(parents=True)
(app/'Contents/Helpers').mkdir()
(app/'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleShortVersionString':version,'CFBundleVersion':build}))
native = app/'Contents/MacOS/ApplePickupMonitor'
native.write_text('#!/bin/sh\nexit 0\n');native.chmod(0o755)
helper = app/'Contents/Helpers/apple-pickup-monitor'
helper.write_text('#!'+sys.executable+'\n'+
    "import json,os,sys\n"+
    "with open(os.environ['FAKE_LOG'],'a') as log: log.write(json.dumps({'tool':'helper','args':sys.argv[1:]})+'\\n')\n"+
    "if os.environ.get('FAKE_HELPER_FAIL_IF_RUN'): sys.exit(99)\n"+
    "print(os.environ.get('FAKE_ENGINE_VERSION',"+repr(version)+"))\n")
helper.chmod(0o755)
''')
        self.tool("otool", "import sys\nprint(sys.argv[-1]+':')\nprint('    /usr/lib/libSystem.B.dylib (compatibility version 1.0.0)')\n")
        self.tool("codesign", "print('Signature=adhoc')\n")
        source = (SCRIPTS / "build-app.sh").read_text()
        # Replace only external build/inspection tools, retaining script logic.
        for name in ["xcodebuild", "otool", "codesign"]:
            source = source.replace("/usr/bin/" + name, shlex.quote(str(self.bin / name)))
        self.script = self.root / "scripts/build-app.sh"
        self.script.write_text(source)

    def tearDown(self):
        self.temp.cleanup()

    def tool(self, name, source):
        path = self.bin / name
        path.write_text("#!" + sys.executable + "\n" + source)
        path.chmod(0o755)

    def run_build(self, **environment):
        return subprocess.run(["/bin/bash", str(self.script)], cwd=self.root,
                              env=dict(self.env, **environment), text=True, capture_output=True)

    def calls(self, name):
        entries = [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []
        return [entry for entry in entries if entry["tool"] == name]

    def metadata(self):
        return plistlib.loads((self.root / "dist/Apple Pickup Monitor.app/Contents/Info.plist").read_bytes())

    def test_default_build_inherits_xcode_version_and_build_number(self):
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        args = self.calls("xcodebuild")[0]["args"]
        self.assertFalse(any(arg.startswith(("MARKETING_VERSION=", "CURRENT_PROJECT_VERSION=")) for arg in args))
        self.assertEqual(self.metadata(), {"CFBundleShortVersionString": "0.5.0", "CFBundleVersion": "42"})
        self.assertEqual(self.calls("helper")[0]["args"], ["--version"])

    def test_explicit_version_and_build_overrides_reach_xcode(self):
        result = self.run_build(APP_VERSION="0.6.1", BUILD_NUMBER="99")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        args = self.calls("xcodebuild")[0]["args"]
        self.assertIn("MARKETING_VERSION=0.6.1", args)
        self.assertIn("CURRENT_PROJECT_VERSION=99", args)
        self.assertEqual(self.metadata(), {"CFBundleShortVersionString": "0.6.1", "CFBundleVersion": "99"})

    def test_invalid_overrides_fail_before_starting_xcode(self):
        for override in [{"APP_VERSION": "0.5"}, {"APP_VERSION": "0.5.0\ninjected"}, {"BUILD_NUMBER": "abc"}]:
            with self.subTest(override=override):
                result = self.run_build(**override)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.calls("xcodebuild"))

    def test_mismatched_helper_does_not_replace_existing_distribution(self):
        original = self.root / "dist/Apple Pickup Monitor.app/original"
        original.parent.mkdir(parents=True)
        original.write_text("keep existing app")
        result = self.run_build(FAKE_ENGINE_VERSION="0.4.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("App version 0.5.0 does not match bundled engine version 0.4.0", result.stderr)
        self.assertEqual(original.read_text(), "keep existing app")

    def test_cross_build_does_not_execute_other_architecture(self):
        result = self.run_build(APPLE_PICKUP_TARGET="x86_64-apple-darwin", FAKE_HELPER_FAIL_IF_RUN="1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.calls("helper"), "Cross-build attempted to execute a helper requiring Rosetta")
        self.assertEqual(self.metadata()["CFBundleShortVersionString"], "0.5.0")


if __name__ == "__main__":
    unittest.main()
