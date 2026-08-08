#!/usr/bin/env python3
"""A fake captive portal, for testing rowt's captive handling without an airport.

Serves three endpoints on 127.0.0.1 (port = argv[1], default 8099), one per
portal behavior the watchdog's classifier must handle (DESIGN.md §11):

  /portal    200 with a lounge-style login page   -> _captive_state = captive
  /redirect  302 to a portal host                 -> _captive_state = captive
  /success   the genuine Apple Success body       -> _captive_state = clear

Point the watchdog's probe at it and run real ticks (NOTE: this toggles the
real system proxy for a few seconds — run it when that's acceptable):

  ROWT_CAPTIVE_URL=http://127.0.0.1:8099/portal  bash bin/rowt watch tick   # drops the proxy
  ROWT_CAPTIVE_URL=http://127.0.0.1:8099/portal  bash bin/rowt watch tick   # idempotent: no new log lines
  ROWT_CAPTIVE_URL=http://127.0.0.1:9/x          bash bin/rowt watch tick   # unknown: stays hands-off
  ROWT_CAPTIVE_URL=http://127.0.0.1:8099/success bash bin/rowt watch tick   # restores the proxy

Stdlib only; nothing is written to disk.
"""

import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

SUCCESS = b"<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>"
PORTAL = b"<HTML><BODY>Welcome to FakeLounge - please log in</BODY></HTML>"


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802 - BaseHTTPRequestHandler's naming
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("Location", "http://portal.fake/login")
            self.end_headers()
            return
        body = PORTAL if self.path == "/portal" else SUCCESS
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def main() -> int:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8099
    print(f"fake portal on http://127.0.0.1:{port}  (/portal /redirect /success)")
    HTTPServer(("127.0.0.1", port), Handler).serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
