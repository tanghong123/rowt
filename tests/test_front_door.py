"""Which implementation the front door runs (rowt #s92, PORTING.md §6.6).

bin/rowt is the file every install puts on PATH. Since 3.9.0 it hands the
whole invocation to rowt-rs unless ROWT_IMPL=bash asks for the shell. The
parity gates cannot see this: their shell side holds itself on the shell, and
their rust side runs rowt-rs directly, so the door itself is tested here.

Each case runs a copy of bin/rowt in a tree of its own, beside a stand-in
rowt-rs that prints what it was handed, in an empty environment. The claims:

- no ROWT_IMPL, or any word but bash, runs rowt-rs, with ROWT_SELF naming
  the front door so a plist either side writes names the same path;
- ROWT_IMPL=bash runs the shell, and so does ROWT_DELEGATED=1 (the marker
  rowt-rs sets when it hands a command back; the door must not bounce it);
- the default takes only a rowt-rs of this install: with none it runs the
  shell, never a rowt-rust found on PATH, which from an unbuilt checkout is
  another version reading the same config;
- ROWT_IMPL=rust, asked for by name, does take one from PATH, and with none
  anywhere is an error, not the shell.

Needs no build: python3 tests/test_front_door.py
"""

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STAND_IN = '#!/bin/sh\nprintf "RUST %s|%s\\n" "$*" "${ROWT_SELF:-}"\n'
VERSION = re.search(r'^ROWT_VERSION="([^"]+)"', (ROOT / "bin/rowt").read_text(), re.M).group(1)


class FrontDoor(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="rowt-front-door-"))
        self.tree = self.root / "tree"
        (self.tree / "bin").mkdir(parents=True)
        (self.tree / "config").mkdir()
        shutil.copy(ROOT / "bin/rowt", self.tree / "bin/rowt")
        for f in (ROOT / "config").glob("*.txt"):
            shutil.copy(f, self.tree / "config" / f.name)
        self.path_dir = self.root / "path"
        self.path_dir.mkdir()
        (self.root / "home").mkdir()

    def tearDown(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)

    def stand_in(self, where: Path, text: str = STAND_IN) -> None:
        where.write_text(text)
        where.chmod(0o755)

    def run_door(self, *args: str, **env: str) -> subprocess.CompletedProcess:
        base = {
            "HOME": str(self.root / "home"),
            "XDG_CONFIG_HOME": str(self.root / "home"),
            "PATH": f"{self.path_dir}:/usr/bin:/bin:/usr/sbin:/sbin",
            "LC_ALL": "C",
            "ROWT_METRICS": "off",
        }
        return subprocess.run(["bash", str(self.tree / "bin/rowt"), *args],
                              env={**base, **env}, capture_output=True, text=True,
                              stdin=subprocess.DEVNULL, timeout=60)

    def assert_rust(self, r: subprocess.CompletedProcess, why: str) -> None:
        self.assertEqual(r.returncode, 0, f"{why}: {r.stderr}")
        self.assertTrue(r.stdout.startswith("RUST version|"), f"{why}: rowt-rs should have answered, got {r.stdout!r}")

    def assert_shell(self, r: subprocess.CompletedProcess, why: str) -> None:
        self.assertEqual(r.returncode, 0, f"{why}: {r.stderr}")
        self.assertEqual(r.stdout.strip(), f"rowt {VERSION}", f"{why}: the shell should have answered")

    def test_the_default_is_rowt_rs(self) -> None:
        self.stand_in(self.tree / "bin/rowt-rs")
        r = self.run_door("version")
        self.assert_rust(r, "no ROWT_IMPL")
        # ROWT_SELF names the front door, resolved, for the plist's Program line.
        self.assertTrue(r.stdout.rstrip().endswith("/tree/bin/rowt"), r.stdout)
        self.assert_rust(self.run_door("version", ROWT_IMPL="rust"), "ROWT_IMPL=rust")
        self.assert_rust(self.run_door("version", ROWT_IMPL="yes"), "a word that names neither")
        self.assert_rust(self.run_door("version", ROWT_IMPL=""), "an empty ROWT_IMPL")

    def test_bash_and_the_delegation_marker_keep_the_shell(self) -> None:
        self.stand_in(self.tree / "bin/rowt-rs")
        self.assert_shell(self.run_door("version", ROWT_IMPL="bash"), "ROWT_IMPL=bash")
        self.assert_shell(self.run_door("version", ROWT_DELEGATED="1"), "ROWT_DELEGATED=1")
        self.assert_shell(self.run_door("version", ROWT_DELEGATED="1", ROWT_IMPL="rust"),
                          "ROWT_DELEGATED=1 with ROWT_IMPL=rust")

    def test_every_install_layout_is_found(self) -> None:
        # brew's libexec/bin/rowt-rs, install.sh's $PREFIX/bin/rowt-rust, a
        # checkout's target/release and target/debug.
        for rel in ("bin/rowt-rs", "bin/rowt-rust", "target/release/rowt-rs", "target/debug/rowt-rs"):
            with self.subTest(layout=rel):
                p = self.tree / rel
                p.parent.mkdir(parents=True, exist_ok=True)
                self.stand_in(p)
                self.assert_rust(self.run_door("version"), rel)
                p.unlink()

    def test_the_default_never_takes_rowt_rs_from_path(self) -> None:
        self.stand_in(self.path_dir / "rowt-rust", '#!/bin/sh\necho "PATH RUST"\n')
        self.assert_shell(self.run_door("version"), "no rowt-rs of this install")
        # By name, PATH is fair: it says which rowt-rs it wants.
        r = self.run_door("version", ROWT_IMPL="rust")
        self.assertEqual((r.returncode, r.stdout.strip()), (0, "PATH RUST"), r.stderr)

    def test_rust_by_name_with_none_anywhere_is_an_error(self) -> None:
        r = self.run_door("version", ROWT_IMPL="rust")
        self.assertEqual(r.returncode, 1)
        self.assertEqual(r.stdout, "")
        self.assertIn("ROWT_IMPL=rust but no rowt-rs found", r.stderr)
        self.assert_shell(self.run_door("version"), "the default with none anywhere")


if __name__ == "__main__":
    unittest.main()
