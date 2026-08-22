"""A static server for looking at the exported site while it is being built.

`python -m http.server` sends no cache headers at all, so a browser applies
its own heuristic freshness and keeps serving yesterday's `style.css` and
`play.js` from memory — which looks exactly like a change that did not land.
This is the same server with `Cache-Control: no-store` on every response, so
a plain reload always shows what is on disk.

    python scripts/devserve.py <port> <directory>
"""

import sys
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer


class Fresh(SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-store, must-revalidate")
        self.send_header("Pragma", "no-cache")
        self.send_header("Expires", "0")
        SimpleHTTPRequestHandler.end_headers(self)

    def log_message(self, fmt, *args):
        # One line per request is noise while watching a build; errors still
        # reach stderr through `log_error`.
        pass


def main() -> int:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8816
    directory = sys.argv[2] if len(sys.argv) > 2 else "out"
    handler = partial(Fresh, directory=directory)
    with ThreadingHTTPServer(("127.0.0.1", port), handler) as httpd:
        print(f"serving {directory} on http://localhost:{port} (no-store)", flush=True)
        httpd.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
