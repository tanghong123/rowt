#!/usr/bin/env python3
"""Regression coverage for Shadowrocket Hysteria2's TLS verification flag."""

import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "sr_import", Path(__file__).with_name("sr-import.py")
)
assert spec and spec.loader
sr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sr)


class Hysteria2Tests(unittest.TestCase):
    def test_insecure_requires_an_explicit_true_value(self):
        for flag in (
            0,
            False,
            None,
            "",
            "0",
            "false",
            "unknown",
            1,
            True,
            "1",
            "true",
            "TRUE",
        ):
            with self.subTest(flag=flag):
                node = {"host": "hy.example", "password": "test", "allowInsecure": flag}
                out = sr._to_hysteria2(node, "HY")
                self.assertEqual(
                    out["tls"]["insecure"], flag in (1, True, "1", "true", "TRUE")
                )


if __name__ == "__main__":
    unittest.main()
