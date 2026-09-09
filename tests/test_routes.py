"""Regression: every HTML page route serves a page — never a middleware.

Scar: @app.get("/") was stacked onto the no_html_cache middleware, so the
root URL demanded a `call_next` query field instead of serving the console.
"""
from fastapi.testclient import TestClient

from forum_agent.server import app

client = TestClient(app)

PAGES = ["/", "/control", "/subtitles", "/insights"]


def test_html_pages_serve_html():
    for path in PAGES:
        r = client.get(path)
        assert r.status_code == 200, f"{path} -> {r.status_code}: {r.text[:120]}"
        assert r.headers["content-type"].startswith("text/html"), path
        assert "call_next" not in r.text, f"{path} routed into a middleware"
        # the no-store middleware must still apply to every HTML page
        assert r.headers.get("cache-control") == "no-store", path


def test_root_is_the_console():
    assert client.get("/").text == client.get("/control").text
