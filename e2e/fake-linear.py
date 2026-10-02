#!/usr/bin/env python3
"""A loopback stand-in for Linear's GraphQL endpoint, for the e2e scenarios.

The fixture is a JSON list of [needle, response] pairs. A request is answered
with the response of the first pair whose needle occurs in its raw body (query
and variables), else a GraphQL error. Each request body is appended to --log as
one JSON line. The chosen port is written to --port-file once listening.
"""

import argparse
import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port-file", required=True)
    parser.add_argument("--log", required=True)
    parser.add_argument("--fixture", required=True)
    args = parser.parse_args()

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:
            length = int(self.headers.get("Content-Length") or 0)
            raw = self.rfile.read(length).decode("utf-8", "replace")
            with open(args.log, "a", encoding="utf-8") as log:
                log.write(json.dumps(json.loads(raw) if raw else None) + "\n")
            # Read on every request, so a scenario can change the answers
            # between steps without restarting the server.
            with open(args.fixture, encoding="utf-8") as fixture:
                pairs = json.load(fixture)
            body = next(
                (response for needle, response in pairs if needle in raw),
                {"errors": [{"message": "fake-linear: unexpected query"}]},
            )
            payload = json.dumps(body).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *_args) -> None:
            pass

    server = HTTPServer(("127.0.0.1", 0), Handler)
    tmp = args.port_file + ".tmp"
    with open(tmp, "w", encoding="utf-8") as port_file:
        port_file.write(str(server.server_address[1]))
    os.replace(tmp, args.port_file)
    server.serve_forever()


if __name__ == "__main__":
    main()
