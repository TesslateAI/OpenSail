"""Profile 1 live tracker. Optional PostgreSQL via DATABASE_URL."""
import os
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

DSN = os.environ.get("DATABASE_URL")


def connect():
    import psycopg

    return psycopg.connect(DSN)


def migrate():
    if not DSN:
        return
    with connect() as conn:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS tasks (id int PRIMARY KEY, title text NOT NULL)"
        )
        conn.execute(
            "INSERT INTO tasks (id, title) VALUES (1, 'tracker') ON CONFLICT DO NOTHING"
        )
        conn.commit()


def seed_title():
    if not DSN:
        return "tracker"
    with connect() as conn:
        row = conn.execute("SELECT title FROM tasks WHERE id = 1").fetchone()
    if not row:
        raise RuntimeError("task row missing")
    return row[0]


def packed_marker():
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "marker.txt")
    try:
        text = open(path, encoding="utf-8").read().strip()
    except OSError:
        text = ""
    return text or "tracker"


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        try:
            if self.path.startswith("/healthz"):
                body = b"ok" if seed_title() else b"empty"
            else:
                body = packed_marker().encode()
        except Exception:
            self.send_error(503)
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        return


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "migrate":
        migrate()
    else:
        # Empty host listens on every local interface. A numeric address
        # literal in this file is rewritten by IP redaction before the agent
        # copies it into the Workspace, and the packed server then fails to bind.
        HTTPServer(("", 3000), Handler).serve_forever()
