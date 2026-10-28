#!/usr/bin/env python3
"""Exercise release gating with fake Apple tools; no credentials or network required."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SECRETS = {
    "MACOS_CERTIFICATE_BASE64": "dGVzdC1jZXJ0aWZpY2F0ZQ==",
    "MACOS_CERTIFICATE_PASSWORD": "test-certificate-password",
    "MACOS_SIGNING_IDENTITY": "Developer ID Application: Test (TESTTEAM)",
    "APPLE_ID": "release@example.test",
    "APPLE_TEAM_ID": "TESTTEAM",
    "APPLE_APP_SPECIFIC_PASSWORD": "test-app-password",
}

FAKE_TOOL = r'''#!/usr/bin/env python3
import json, os, pathlib, plistlib, shutil, sys, zipfile
name, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
with open(os.environ["SSMUX_TEST_LOG"], "a") as log:
    log.write(json.dumps([name, args]) + "\n")
failure = os.environ.get("SSMUX_TEST_FAIL")
if failure and " ".join([name, *args]).startswith(failure + " "):
    sys.exit(1)
if name == "security" and args[0] == "create-keychain":
    pathlib.Path(args[-1]).touch()
elif name == "plutil":
    with open(args[-1], "rb") as source:
        plistlib.load(source)
elif name == "xcrun" and args[:2] == ["notarytool", "submit"]:
    print(json.dumps({"status": os.environ.get("SSMUX_TEST_NOTARY_STATUS", "Accepted"), "id": "test-submission"}))
elif name == "ditto":
    source, destination = map(pathlib.Path, args[-2:])
    if "-c" in args:
        with zipfile.ZipFile(destination, "w") as archive:
            for path in source.rglob("*"):
                if path.is_file():
                    archive.write(path, path.relative_to(source.parent))
    else:
        shutil.copytree(source, destination)
'''


class MacOSReleaseTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ssmux-release-test-")
        self.addCleanup(self.temp.cleanup)
        self.project = Path(self.temp.name)
        scripts = self.project / "scripts"
        scripts.mkdir()
        for name in ["package-macos-menubar.sh", "notarize-macos.sh"]:
            shutil.copy2(Path(__file__).with_name(name), scripts / name)
        (self.project / "assets").mkdir()
        (self.project / "assets/AppIcon.icns").write_bytes(b"test-icon")
        self.binary = self.project / "ssmux"
        self.binary.write_text("#!/bin/sh\necho 'ssmux 0.4.0'\n")
        self.binary.chmod(0o755)
        fake_bin = self.project / "bin"
        fake_bin.mkdir()
        for name in ["security", "codesign", "plutil", "xcrun", "ditto", "spctl"]:
            tool = fake_bin / name
            tool.write_text(FAKE_TOOL)
            tool.chmod(0o755)
        self.log = self.project / "commands.jsonl"
        self.output = self.project / "release.zip"
        self.env = {
            key: value for key, value in os.environ.items()
            if key not in SECRETS and not key.startswith("SSMUX_SIGNING_")
        }
        self.env.update(SECRETS)
        self.env.update(PATH=f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
                        TMPDIR=str(self.project), SSMUX_TEST_LOG=str(self.log))

    def run_script(self, name="notarize-macos.sh"):
        args = ["sh", str(self.project / "scripts" / name), str(self.binary)]
        if name == "notarize-macos.sh":
            args.append(str(self.output))
        return subprocess.run(args, env=self.env, text=True, capture_output=True, timeout=15)

    def commands(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def assert_cleaned(self):
        self.assertFalse(list(self.project.glob("ssmux-signing.*")))
        self.assertTrue(any(name == "security" and args[0] == "delete-keychain"
                            for name, args in self.commands()))

    def test_local_package_remains_ad_hoc_and_verified(self):
        result = self.run_script("package-macos-menubar.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        signing = [args for name, args in self.commands() if name == "codesign"]
        self.assertIn("-", signing[0])
        self.assertIn("--verify", signing[1])

    def test_missing_secrets_stop_before_any_signing_or_submission(self):
        for secret in SECRETS:
            with self.subTest(secret=secret):
                saved = self.env.pop(secret)
                result = self.run_script()
                self.env[secret] = saved
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(secret, result.stderr)
                self.assertEqual(self.commands(), [])
                self.assertFalse(self.output.exists())

    def test_wrong_certificate_type_is_rejected(self):
        self.env["MACOS_SIGNING_IDENTITY"] = "Apple Development: Test"
        self.assertNotEqual(self.run_script().returncode, 0)
        self.assertEqual(self.commands(), [])

    def test_only_accepted_and_verified_app_produces_archive(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.output.is_file())
        commands = self.commands()
        signatures = [args for name, args in commands if name == "codesign" and "--sign" in args]
        self.assertEqual(len(signatures), 2)
        self.assertTrue(all("runtime" in args and "--timestamp" in args and "--keychain" in args
                            for args in signatures))
        submit = next(args for name, args in commands
                      if name == "xcrun" and args[:2] == ["notarytool", "submit"])
        self.assertIn("--wait", submit)
        self.assertEqual(submit[submit.index("--timeout") + 1], "20m")
        self.assertTrue(any(name == "xcrun" and args[:2] == ["stapler", "validate"]
                            for name, args in commands))
        self.assertTrue(any(name == "spctl" for name, _ in commands))
        self.assert_cleaned()

    def test_rejected_notarization_never_publishes_or_staples(self):
        self.env["SSMUX_TEST_NOTARY_STATUS"] = "Invalid"
        self.assertNotEqual(self.run_script().returncode, 0)
        self.assertFalse(self.output.exists())
        self.assertFalse(any(name == "xcrun" and args[0] == "stapler"
                             for name, args in self.commands()))
        self.assert_cleaned()

    def test_tool_failures_never_produce_release_archive_and_clean_credentials(self):
        for operation in ["security import", "codesign --force", "xcrun notarytool submit", "xcrun stapler staple",
                          "xcrun stapler validate", "spctl --assess --type"]:
            with self.subTest(operation=operation):
                self.env["SSMUX_TEST_FAIL"] = operation
                result = self.run_script()
                self.assertNotEqual(result.returncode, 0, operation)
                self.assertFalse(self.output.exists())
                self.assert_cleaned()


if __name__ == "__main__":
    unittest.main()
