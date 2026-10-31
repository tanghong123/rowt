#!/usr/bin/env python3
"""Shadowrocket Hysteria2 conversion regressions (no live credentials)."""

import importlib.util
import json
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "sr", Path(__file__).with_name("sr-import.py")
)
assert spec and spec.loader
sr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sr)


def node(**extra):
    return dict(
        type="Hysteria2",
        host="hy.example",
        port="8443",
        password="secret",
        tls=False,
        alpn="h3",
        allowInsecure=1,
        obfs="none",
        plugin="none",
        pluginParam={"NS.keys": [], "NS.objects": [], "$class": {}},
        **extra,
    )


class Hysteria2Tests(unittest.TestCase):
    def test_pin_is_skipped_only_with_explicit_allow_insecure(self):
        for flag in (0, "0", False, None, "false", "bad"):
            n = node(hpkp="ab" * 32)
            n["allowInsecure"] = flag
            with self.subTest(flag=flag):
                self.assertIsNone(sr._to_hysteria2(n, "HY"))
        for flag in (1, "1", True, "true"):
            n = node(hpkp="ab" * 32)
            n["allowInsecure"] = flag
            with self.subTest(flag=flag):
                out = sr._to_hysteria2(n, "HY")
                self.assertTrue(out["tls"]["insecure"])
                self.assertNotIn("certificate_sha256", out["tls"])
                self.assertNotIn("certificate_public_key_sha256", out["tls"])

    def test_archive_to_review_and_safe_skip_warning(self):
        objects = ["$null"]
        n = node()
        archived = {}
        for k, v in n.items():
            objects.append({"NS.string": v} if isinstance(v, str) else v)
            archived[k] = plistlib.UID(len(objects) - 1)
        objects.append(archived)
        pinned = dict(n, hpkp="ab" * 32)
        objects.append(pinned)
        objects.append(dict(pinned, allowInsecure=0))
        with tempfile.TemporaryDirectory(prefix="rowt-sr-test-") as directory:
            store = Path(directory) / "ServerManager"
            conf = Path(directory) / "rules.conf"
            store.write_bytes(
                plistlib.dumps({"$objects": objects}, fmt=plistlib.FMT_BINARY)
            )
            conf.write_text("[Rule]\nDOMAIN-SUFFIX,example.com,PROXY\n")
            result = subprocess.run(
                [
                    sys.executable,
                    str(Path(sr.__file__)),
                    "--store",
                    str(store),
                    "--conf",
                    str(conf),
                ],
                capture_output=True,
                text=True,
                check=True,
            )
        review = json.loads(result.stdout)
        self.assertEqual(len(review["servers"]), 2)
        self.assertEqual(review["servers"][0]["type"], "hysteria2")
        self.assertEqual(review["skipped"], {"Hysteria2": 1})
        self.assertEqual(review["proxy_domains"], ["example.com"])
        self.assertEqual(
            result.stderr,
            "warning: Shadowrocket Hysteria2 imported with allowInsecure=1; certificate pin (hpkp) ignored (temporary bypass; server identity NOT verified)\n"
            "warning: skipped Shadowrocket Hysteria2: "
            "certificate pin (hpkp) cannot be preserved by sing-box 1.13\n",
        )

    def test_unpinned_string_false_keeps_verification(self):
        n = node()
        n["allowInsecure"] = "0"
        self.assertFalse(sr._to_hysteria2(n, "HY")["tls"]["insecure"])


if __name__ == "__main__":
    unittest.main()
