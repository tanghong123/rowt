"""The escape servers' names: checked over DoH by rowt, followed live by sing-box.

The feature rests on three claims, each tested here:

1. rowt (both CLIs, one implementation behind them) asks EVERY DoH source,
   refuses what no public server answers with, and writes the answer
   sing-box gets to escape-hosts. When the sources disagree it takes no one
   source's word: the delay probe through the server itself settles it, else
   the last good answer stays, else the system resolver's when not refused.
   A watchdog tick follows a provider's new address WITHOUT restarting the
   router. Against two local fake DoH servers and a fake engine, through the
   isolated subscription sandbox. The incident's shape in documentation
   ranges: one source answers 203.0.113.66 (the "sinkhole"), the other
   198.51.100.7 (the real entry).

2. The probe is the real delay test, not a connect: both candidates ACCEPT a
   connection, only one speaks the server's protocol, and that one is used —
   on a fresh box with no last good answer. Real sing-box `tools fetch`
   through a twin SOCKS member, against a local acceptor and a local SOCKS
   server on two of this machine's own addresses.

3. A running sing-box re-reads the hosts file when it changes, so a rewrite
   reaches connections with no reload — on a private port with a config of
   its own, never the router this machine runs.

2 and 3 are skipped when no sing-box binary is found (ROWT_TEST_SINGBOX, the
repo's bin/, or the Homebrew install's bundled copy).

Run after cargo build --release --workspace: python3 tests/test_escape_dns.py
"""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import unittest
from urllib.parse import parse_qs, urlsplit

from subscription_sandbox import Sandbox


ROOT = Path(__file__).resolve().parents[1]


class FakeDoH:
    """DoH's JSON API, standing for one provider: `answers[name]` is its say."""

    def __init__(self):
        self.answers = {}  # name -> [ipv4…]; absent = NXDOMAIN
        fake = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                name = parse_qs(urlsplit(self.path).query).get("name", [""])[0]
                addrs = fake.answers.get(name)
                body = {"Status": 3} if addrs is None else {
                    "Status": 0,
                    "Answer": [{"name": name + ".", "type": 1, "TTL": 60, "data": a} for a in addrs],
                }
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps(body).encode())

            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_port
        self.url = f"http://127.0.0.1:{self.port}/resolve"
        self.label = f"127.0.0.1:{self.port}"  # how rowt names a source
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


# The sandbox's fake engine, plus `tools fetch` — the probe — answering from a
# pass list: the twin's address passes when it is listed in probe-pass.
FAKE_ENGINE = """#!/bin/bash
case "$1" in
  version) echo 'sing-box version 1.13.14' ;;
  check) exit 0 ;;
  run) printf 'run\\n' >> "$SUB_TEST_ROOT/router-events"; echo "$$" >> "$SUB_TEST_ROOT/engine-pids"; exec /bin/sleep 300 ;;
  tools)
    printf 'probe\\n' >> "$SUB_TEST_ROOT/router-events"
    shift; cfg=""; tag=""
    while [ "$#" -gt 0 ]; do case "$1" in -c) cfg="$2"; shift ;; -o) tag="$2"; shift ;; esac; shift; done
    addr="$(jq -r --arg t "$tag" '.outbounds[] | select(.tag == $t) | .server' "$cfg")"
    grep -qxF "$addr" "$SUB_TEST_ROOT/probe-pass" 2>/dev/null ;;
esac
"""


class EscapeDnsTests(unittest.TestCase):
    def sandbox(self, implementation, doh_a, doh_b, extra=""):
        box = Sandbox(implementation)
        (box.cfg / "escape-dns.txt").write_text(f"doh {doh_a.url} {doh_b.url}\n{extra}")
        box.env.update({"SUB_TEST_DOH": f"{doh_a.port} {doh_b.port}", "ROWT_ESCAPE_DNS_MAX_AGE": "0"})
        box.executable(box.cfg / "bin/sing-box", FAKE_ENGINE)
        return box

    def row(self, box):
        return next(r.split("\t") for r in (box.cfg / "escape-dns.tsv").read_text().splitlines()
                    if r.startswith("node.example\t"))

    def given(self, box):
        lines = (box.cfg / "escape-hosts").read_text().splitlines()
        return [line.split()[0] for line in lines if not line.startswith("#")]

    def probes(self, box):
        events = box.root / "router-events"
        return events.read_text().splitlines().count("probe") if events.exists() else 0

    def test_refusal_agreement_and_a_moved_entry_followed_without_a_restart(self):
        for implementation in ("bash", "rust"):
            with self.subTest(implementation=implementation):
                a, b = FakeDoH(), FakeDoH()
                try:
                    with self.sandbox(implementation, a, b) as box:
                        passes = box.root / "probe-pass"
                        # One source hands out a fake-IP address (refused by
                        # category: no say), the other the truth. A fresh box:
                        # the first answer is taken.
                        a.answers["node.example"] = ["198.18.0.9"]
                        b.answers["node.example"] = ["192.0.2.10"]
                        box.run("render")
                        self.assertEqual(self.row(box)[1:6],
                                         ["ok", "192.0.2.10", b.label, f"198.18.0.9=fake-IP range@{a.label}", "-"])
                        self.assertEqual(self.given(box), ["192.0.2.10"])
                        host = json.loads((box.cfg / "host.json").read_text())
                        member = next(o for o in host["outbounds"] if o.get("server") == "node.example")
                        self.assertEqual(member["domain_resolver"],
                                         {"server": "dns-escape", "strategy": "ipv4_only", "disable_cache": True})
                        self.assertIn({"type": "hosts", "tag": "dns-escape", "path": [str(box.cfg / "escape-hosts")]},
                                      host["dns"]["servers"])

                        # The provider moves its entry and both sources agree.
                        # It differs from the last good answer, so it passes the
                        # probe first; a watchdog tick then follows it into
                        # escape-hosts with no restart — sing-box reads the file
                        # live. One probe for the move, none on the next tick.
                        box.start_router()
                        a.answers["node.example"] = b.answers["node.example"] = ["192.0.2.20"]
                        passes.write_text("192.0.2.20\n")
                        box.run("watch", "tick")
                        self.assertEqual(self.row(box)[1:4], ["moved", "192.0.2.20", f"{a.label}+{b.label}"])
                        self.assertEqual(self.given(box), ["192.0.2.20"])
                        box.run("watch", "tick")
                        self.assertEqual(self.row(box)[1:3], ["ok", "192.0.2.20"])
                        self.assertEqual((box.starts(), self.probes(box)), (0, 1))

                        # One source unreachable and its answer new: not taken
                        # on its word. It is held until it passes the probe.
                        a.answers.pop("node.example")
                        b.answers["node.example"] = ["192.0.2.21"]
                        box.run("watch", "tick")
                        self.assertEqual(self.row(box)[1:4], ["held", "192.0.2.20", "last good"])
                        self.assertEqual(self.given(box), ["192.0.2.20"])
                        passes.write_text("192.0.2.21\n")
                        box.run("watch", "tick")
                        self.assertEqual(self.row(box)[1:3], ["moved", "192.0.2.21"])
                        self.assertEqual(self.given(box), ["192.0.2.21"])
                        b.answers["node.example"] = ["127.0.0.1"]
                        box.run("watch", "tick")
                        self.assertEqual(self.row(box)[1:3], ["refused", "192.0.2.21"])

                        # Both poisoned by category: the last good address stays,
                        # and status says so.
                        a.answers["node.example"] = ["10.1.1.1"]
                        box.run("watch", "tick")
                        self.assertEqual(self.given(box), ["192.0.2.21"])
                        status = box.run("status")
                        self.assertIn(f"escape dns:   1 server name(s), each asked of {a.label} and {b.label}: 1 refused",
                                      status)
                        self.assertIn(f"node.example: REFUSED 10.1.1.1 (private, from {a.label}), 127.0.0.1 (loopback, "
                                      f"from {b.label}) — still using 192.0.2.21, the last good answer", status)
                        self.assertEqual(box.starts(), 0)
                finally:
                    a.close()
                    b.close()

    def test_disagreeing_sources_are_settled_by_the_probe_or_keep_the_last_good_answer(self):
        for implementation in ("bash", "rust"):
            with self.subTest(implementation=implementation):
                a, b = FakeDoH(), FakeDoH()
                try:
                    with self.sandbox(implementation, a, b) as box:
                        # The incident: A repeats the hijack, B has the real
                        # entry. A fresh box, so no last good answer — and only
                        # B's address passes the probe through the server.
                        a.answers["node.example"] = ["203.0.113.66"]
                        b.answers["node.example"] = ["198.51.100.7"]
                        (box.root / "probe-pass").write_text("198.51.100.7\n")
                        out = box.run("render")
                        self.assertEqual(self.row(box)[1:4], ["probed", "198.51.100.7", b.label])
                        self.assertEqual(self.given(box), ["198.51.100.7"])
                        self.assertEqual(self.probes(box), 2, "one probe per candidate")
                        self.assertIn(f"warning: escape server node.example: DoH sources disagree (203.0.113.66 from "
                                      f"{a.label}; 198.51.100.7 from {b.label}) — 198.51.100.7 (from {b.label}) "
                                      f"answered the probe through the server, so it is used", out)

                        # A new disagreement neither candidate passes: the last
                        # good answer stays. One source's word never replaces it.
                        b.answers["node.example"] = ["198.51.100.8"]
                        (box.root / "probe-pass").write_text("")
                        box.run("render")
                        self.assertEqual(self.row(box)[1:4], ["disputed", "198.51.100.7", "last good"])
                        self.assertEqual(self.given(box), ["198.51.100.7"])
                        # …nor when both pass: still ambiguous.
                        (box.root / "probe-pass").write_text("203.0.113.66\n198.51.100.8\n")
                        box.run("render")
                        self.assertEqual(self.given(box), ["198.51.100.7"])
                        status = box.run("status")
                        self.assertIn("1 disputed", status)
                        self.assertIn("the probe did not settle it, so 198.51.100.7, the last good answer, stays", status)

                        # They agree again, on an answer that is not the last
                        # good one: it passes the probe, so it moves.
                        a.answers["node.example"] = ["198.51.100.8"]
                        box.run("render")
                        self.assertEqual(self.row(box)[1:3], ["moved", "198.51.100.8"])
                finally:
                    a.close()
                    b.close()

    def test_a_system_resolver_tie_break_is_used_but_never_trusted(self):
        for implementation in ("bash", "rust"):
            with self.subTest(implementation=implementation):
                a, b = FakeDoH(), FakeDoH()
                try:
                    with self.sandbox(implementation, a, b) as box:
                        # A fresh box, the sources disagree, the probe fails both
                        # (the server is down, say), and the system resolver
                        # answers: its answer is used…
                        a.answers["node.example"] = ["203.0.113.66"]
                        b.answers["node.example"] = ["198.51.100.7"]
                        dig = box.answers / "dig_short_time_2_tries_1_node_example_A"
                        dig.write_text("203.0.113.66\n")
                        box.run("render")
                        self.assertEqual(self.row(box)[1:4], ["disputed", "203.0.113.66", "system resolver"])
                        # …but on the next refresh it is not "the last good
                        # answer": the system resolver is asked again, and with
                        # no answer from it the server gets nothing.
                        dig.unlink()
                        box.run("render")
                        self.assertEqual(self.row(box)[1:4], ["disputed", "-", "-"])
                        self.assertEqual(self.given(box), [])
                finally:
                    a.close()
                    b.close()


def find_singbox():
    for c in (os.environ.get("ROWT_TEST_SINGBOX"), ROOT / "bin/sing-box",
              "/opt/homebrew/opt/rowt/libexec/bin/sing-box", "/usr/local/opt/rowt/libexec/bin/sing-box"):
        if c and Path(c).is_file() and os.access(c, os.X_OK):
            return str(c)
    return None


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def local_address():
    """An IPv4 address of this machine other than loopback, or None."""
    # By path: a non-interactive ssh shell's PATH can lack /sbin.
    ifconfig = "/sbin/ifconfig" if os.access("/sbin/ifconfig", os.X_OK) else "ifconfig"
    out = subprocess.run([ifconfig], capture_output=True, text=True).stdout
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] == "inet" and not parts[1].startswith("127."):
            return parts[1]
    return None


def answers_here(host):
    """Whether a listener of this test's own on `host` gets a connection's data.

    With the macOS application firewall on, a listener on the LAN address
    completes the handshake but never receives data (Homebrew's Python is not
    on the firewall's allowed list), while the same listener on 127.0.0.1
    does. The drill below would then read as rowt failing to probe."""
    srv = socket.socket()
    srv.bind((host, 0))
    srv.listen(1)
    srv.settimeout(3)

    def serve():
        try:
            c, _ = srv.accept()
            with c:
                c.settimeout(3)
                c.sendall(recv_exact(c, 4))
        except OSError:
            pass

    t = threading.Thread(target=serve, daemon=True)
    t.start()
    try:
        with socket.create_connection((host, srv.getsockname()[1]), 3) as c:
            c.settimeout(3)
            c.sendall(b"ping")
            return recv_exact(c, 4) == b"ping"
    except (OSError, ConnectionError):
        return False
    finally:
        srv.close()
        t.join(4)


def recv_exact(c, n):
    data = b""
    while len(data) < n:
        chunk = c.recv(n - len(data))
        if not chunk:
            raise ConnectionError("closed")
        data += chunk
    return data


class Listener:
    """A TCP listener on (host, port) serving each connection on a thread."""

    def __init__(self, host, port, serve):
        self.sock = socket.socket()
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind((host, port))
        self.sock.listen(16)
        self.accepted = 0
        self.serve = serve
        self.thread = threading.Thread(target=self.loop, daemon=True)
        self.thread.start()

    def loop(self):
        while True:
            try:
                c, _ = self.sock.accept()
            except OSError:
                return
            self.accepted += 1
            threading.Thread(target=self.serve, args=(c,), daemon=True).start()

    def close(self):
        self.sock.close()


def sinkhole(c):
    """Accepts, reads what it is sent, answers nothing useful."""
    c.settimeout(2)
    try:
        c.recv(1024)
        c.sendall(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
    except OSError:
        pass
    c.close()


def socks5(c):
    """Just enough SOCKS5 (no auth, CONNECT) to carry the probe."""
    try:
        c.settimeout(5)
        _, n = recv_exact(c, 2)
        recv_exact(c, n)
        c.sendall(b"\x05\x00")
        _, _, _, atyp = recv_exact(c, 4)
        if atyp == 1:
            host = socket.inet_ntoa(recv_exact(c, 4))
        else:
            host = recv_exact(c, recv_exact(c, 1)[0]).decode()
        port = int.from_bytes(recv_exact(c, 2), "big")
        r = socket.create_connection((host, port), 5)
        c.sendall(b"\x05\x00\x00\x01\x00\x00\x00\x00\x00\x00")

        def pump(src, dst):
            try:
                while (data := src.recv(65536)):
                    dst.sendall(data)
            except OSError:
                pass
            for s in (src, dst):
                try:
                    s.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

        back = threading.Thread(target=pump, args=(r, c), daemon=True)
        back.start()
        pump(c, r)
        back.join(5)
        r.close()
    except (OSError, ConnectionError):
        pass
    c.close()


class Generate204(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(204)
        self.end_headers()

    do_HEAD = do_GET

    def log_message(self, *_args):
        pass


class ProbeDrill(unittest.TestCase):
    """Both candidates accept a connection; only one is the server."""

    @unittest.skipUnless(find_singbox(), "no sing-box binary (set ROWT_TEST_SINGBOX)")
    def test_only_the_candidate_that_passes_the_delay_probe_is_used_on_a_fresh_box(self):
        lan = local_address()
        if not lan:
            self.skipTest("no non-loopback IPv4 address to stand for the second candidate")
        if not answers_here(lan):
            self.skipTest(f"a listener on this Mac's own address {lan} gets no data, so the second "
                          "candidate cannot answer: is the application firewall on? "
                          "(/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate)")
        for implementation in ("bash", "rust"):
            with self.subTest(implementation=implementation):
                a, b = FakeDoH(), FakeDoH()
                web = ThreadingHTTPServer(("127.0.0.1", 0), Generate204)
                threading.Thread(target=web.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True).start()
                port = free_port()
                hole, server = Listener("127.0.0.1", port, sinkhole), Listener(lan, port, socks5)
                try:
                    with Sandbox(implementation) as box:
                        real = box.root / "sing-box.real"
                        shutil.copy(find_singbox(), real)
                        box.executable(box.cfg / "bin/sing-box", FAKE_ENGINE.replace(
                            "  tools)\n", f'  tools) exec "{real}" "$@" ;;\n  never)\n'))
                        member = {"type": "socks", "tag": "entry", "server": "node.example",
                                  "server_port": port, "version": "5"}
                        (box.cfg / "servers.json").write_text(json.dumps([member]))
                        # Both are this machine's own addresses, which no public
                        # server has: allowed by name for the drill.
                        (box.cfg / "escape-dns.txt").write_text(
                            f"doh {a.url} {b.url}\nallow node.example 127.0.0.1/32 {lan}/32\n")
                        box.env.update({
                            "SUB_TEST_DOH": f"{a.port} {b.port}", "ROWT_ESCAPE_DNS_MAX_AGE": "0",
                            # Twins bind the interface the router would; loopback here.
                            "ROWT_IFACE": "lo0",
                            "ROWT_HEALTH_URL": f"http://127.0.0.1:{web.server_port}/generate_204",
                            "ROWT_HEALTH_TIMEOUT": "4",
                        })
                        a.answers["node.example"] = ["127.0.0.1"]  # the sinkhole
                        b.answers["node.example"] = [lan]          # the server
                        started = time.monotonic()
                        box.run("render")
                        took = time.monotonic() - started
                        row = next(r.split("\t") for r in (box.cfg / "escape-dns.tsv").read_text().splitlines()
                                   if r.startswith("node.example\t"))
                        self.assertEqual(row[1:4], ["probed", lan, b.label])
                        self.assertGreaterEqual(hole.accepted, 1, "the sinkhole never even got a connection")
                        self.assertGreaterEqual(server.accepted, 1)
                        print(f"\n  {implementation}: render with a disputed name took {took:.1f}s; "
                              f"sinkhole accepted {hole.accepted}, server {server.accepted}")
                finally:
                    hole.close()
                    server.close()
                    web.shutdown()
                    web.server_close()
                    a.close()
                    b.close()


class HostsFollowDrill(unittest.TestCase):
    @unittest.skipUnless(find_singbox(), "no sing-box binary (set ROWT_TEST_SINGBOX)")
    def test_a_running_sing_box_follows_a_rewritten_hosts_file(self):
        with tempfile.TemporaryDirectory(prefix="rowt-hosts-drill-") as tmp:
            d = Path(tmp)
            sb = d / "sing-box"
            shutil.copy(find_singbox(), sb)
            (d / "index.html").write_text("drill-ok\n")
            hosts = d / "escape-hosts"
            hosts.write_text("127.0.0.1 entry.example\n")
            web_port, mixed_port = free_port(), free_port()
            (d / "cfg.json").write_text(json.dumps({
                "log": {"level": "warn"},
                "dns": {"servers": [{"type": "local", "tag": "local"},
                                    {"type": "hosts", "tag": "dns-escape", "path": [str(hosts)]}]},
                "inbounds": [{"type": "mixed", "tag": "in", "listen": "127.0.0.1", "listen_port": mixed_port}],
                # What rowt gives a checked member, on an outbound that needs no server.
                "outbounds": [{"type": "direct", "tag": "probe",
                               "domain_resolver": {"server": "dns-escape", "strategy": "ipv4_only", "disable_cache": True}}],
                "route": {"final": "probe", "default_domain_resolver": "local"},
            }))
            web = subprocess.Popen(["python3", "-m", "http.server", str(web_port), "--bind", "127.0.0.1"], cwd=d,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            router = subprocess.Popen([str(sb), "run", "-c", str(d / "cfg.json")],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                def fetch():
                    r = subprocess.run(["curl", "-sS", "-m", "3", "--noproxy", "", "-x", f"http://127.0.0.1:{mixed_port}",
                                        f"http://entry.example:{web_port}/index.html"],
                                       capture_output=True, text=True)
                    return r.stdout.strip()

                def eventually(want, timeout=12):
                    end = time.monotonic() + timeout
                    while time.monotonic() < end:
                        if fetch() == want:
                            return True
                        time.sleep(1)
                    return False

                self.assertTrue(eventually("drill-ok"), "the first address was never used")
                # TEST-NET-1: never routed. The rewrite must reach the RUNNING
                # router — sing-box re-stats the file at most every 5 s.
                tmpf = d / "escape-hosts.n"
                tmpf.write_text("192.0.2.1 entry.example\n")
                tmpf.rename(hosts)
                self.assertTrue(eventually(""), "sing-box kept the old address after the file changed")
                tmpf.write_text("127.0.0.1 entry.example\n")
                tmpf.rename(hosts)
                self.assertTrue(eventually("drill-ok"), "sing-box did not pick the address back up")
            finally:
                for p in (router, web):
                    p.terminate()
                    p.wait(timeout=10)


# The shell asks its companion rowt-rs for the escape-name check. A companion
# that does not answer the call — a build from before the arm existed, found
# beside a newer bin/rowt, as a fleet that bundles its own binaries can end up
# with — must cost the render the check, not the render (rowt #s33). Shaped
# like the real thing: a rowt-rs 3.5.19 prints the error on stderr and its
# usage page on stdout, and exits 1.
OLD_COMPANION = """#!/bin/bash
case "$1" in
  version) echo 'rowt 3.5.19' ;;
  *) echo "error: unknown command: $1" >&2
     printf '\\nrowt — split traffic three ways from one local proxy (127.0.0.1:7890)\\n  escape  domains in escape-domains.txt\\n'
     exit 1 ;;
esac
"""


class CompanionTests(unittest.TestCase):
    def tree(self, box, companion):
        """bin/rowt in a tree of its own, so `_rust_cli` finds what this test put beside it."""
        tree = box.root / "tree"
        (tree / "bin").mkdir(parents=True)
        (tree / "config").mkdir()
        shutil.copy(ROOT / "bin/rowt", tree / "bin/rowt")
        for f in (ROOT / "config").glob("*.txt"):
            shutil.copy(f, tree / "config" / f.name)
        if companion is not None:
            box.executable(tree / "bin/rowt-rs", companion)
        return tree

    def render(self, box, tree, path=None):
        env = dict(box.env)
        if path is not None:
            env["PATH"] = path
        return subprocess.run(["bash", str(tree / "bin/rowt"), "render"], env=env,
                              capture_output=True, text=True, timeout=120)

    def dns_tags(self, box):
        return [s["tag"] for s in json.loads((box.cfg / "host.json").read_text())["dns"]["servers"]]

    def test_a_companion_without_the_arm_costs_the_check_not_the_render(self):
        with Sandbox("bash") as box:
            box.executable(box.cfg / "bin/sing-box", FAKE_ENGINE)
            tree = self.tree(box, OLD_COMPANION)
            r = self.render(box, tree)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            # (No names to guard in this fixture, so a working companion gives
            # the same host.json; the exit status and the lines below carry it.)
            self.assertNotIn("dns-escape", self.dns_tags(box))
            host = json.loads((box.cfg / "host.json").read_text())
            self.assertFalse([o for o in host["outbounds"] if (o.get("domain_resolver") or {}).get("server") == "dns-escape"])
            said = [line for line in r.stderr.splitlines() if "did not answer the escape-name check" in line]
            self.assertEqual(len(said), 1, r.stderr)  # refresh and guarded both failed; one line
            self.assertIn(str((tree / "bin/rowt-rs").resolve()), said[0])  # $HERE is cd -P resolved
            self.assertIn("(rowt 3.5.19) did not answer the escape-name check (exit 1)", said[0])
            self.assertNotIn("split traffic three ways", r.stdout)  # the usage page stays out of the render's output
            self.assertNotIn("unknown command", r.stderr)  # and the companion's own complaint is dropped
            self.assertNotIn("argjson", r.stderr)
            # status says so in its own row instead of losing the line.
            st = subprocess.run(["bash", str(tree / "bin/rowt"), "status"], env=box.env,
                                capture_output=True, text=True, timeout=120)
            rows = [line for line in st.stdout.splitlines() if line.startswith("escape dns:")]
            self.assertEqual(len(rows), 1, st.stdout + st.stderr)
            self.assertIn("not checked — rowt-rs at", rows[0])
            self.assertIn("(rowt 3.5.19) did not answer (exit 1)", rows[0])

    @unittest.skipUnless(shutil.which("jq"), "no jq on PATH")
    def test_no_companion_at_all_renders_without_the_check(self):
        with Sandbox("bash") as box:
            box.executable(box.cfg / "bin/sing-box", FAKE_ENGINE)
            tree = self.tree(box, None)
            # No rowt-rust on PATH either: only the recorder shims, jq, and the OS.
            tools = box.root / "tools"
            tools.mkdir()
            (tools / "jq").symlink_to(shutil.which("jq"))
            path = os.pathsep.join([str(box.shims), str(tools), "/usr/bin", "/bin", "/usr/sbin", "/sbin"])
            r = self.render(box, tree, path)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertNotIn("dns-escape", self.dns_tags(box))
            self.assertNotIn("escape-name check", r.stderr)


if __name__ == "__main__":
    unittest.main()
