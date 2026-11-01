#!/usr/bin/env python3
"""Regenerates the settings page screenshots in docs/screenshots/.

Serves deskflow-proto/src/web/index.html from a small mock of the bridge's
API (made-up but realistic data, nothing from a real device) and photographs
it with headless Chromium.

    python3 -m venv /tmp/shots && /tmp/shots/bin/pip install playwright
    /tmp/shots/bin/playwright install chromium
    /tmp/shots/bin/python tools/screenshots.py
"""

import json
import os
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PAGE = ROOT / "deskflow-proto/src/web/index.html"
OUT = ROOT / "docs/screenshots"

CLIENT_FP = "3A:9C:41:07:E2:5B:D8:16:70:AF:C3:2E:94:6D:0B:F1:58:A7:1C:E4:82:3D:96:4F:B0:25:7A:CE:61:D3:19:8E"
SERVER_FP = "C1:7E:03:B5:4A:92:F8:2D:66:19:AB:E0:73:5C:8F:D4:31:A6:0E:97:42:BD:68:F3:1A:C5:79:20:8B:E6:54:9D"

SETTINGS = {
    "screen_name": "laptop", "screen_width": 1920, "screen_height": 1200,
    "ble_name": "Deskflow Bridge", "key_source": "mac", "mouse_mode": "absolute",
    "typing_layout": "us", "typing_rate": 40, "tls": "pinned", "fingerprint": SERVER_FP,
    "button1_short": "type", "button1_long": "stop",
    "button2_short": "next", "button2_long": "release",
    "button3_short": "lock", "button3_long": "restart",
}

STATE = {
    "settings": SETTINGS,
    "status": {
        "deskflow": "active", "ble_connected": True, "ble_bonded": True,
        "uptime_s": 3 * 3600 + 27 * 60 + 14, "clipboard_chars": 182,
        "clipboard_truncated": False, "clipboard_max": 65536,
        "firmware": "9682a73", "updates": True, "typing": None,
        "tls_handshake_ms": 1310,
        "client_fingerprint": CLIENT_FP,
        "client_db_line": "v2:sha256:" + CLIENT_FP.replace(":", "").lower(),
        "server_fingerprint": SERVER_FP,
    },
    "diag": {
        "ble_rssi": -52, "ble_interval_us": 15000, "ble_connections": 3,
        "deskflow_sessions": 2, "latency_avg_us": 2900, "latency_max_us": 11400,
        "tls_path_avg_us": 1300, "tls_path_max_us": 4800, "backlog_avg": 120,
        "backlog_max": 425, "in_chip_avg": 1, "in_chip_max": 2, "flow_timeouts": 0,
        "heap_free": 21 * 1024, "heap_size": 32 * 1024,
        "stack0_free": 18 * 1024, "stack1_free": 14 * 1024,
    },
    "laptops": {"list": [
        {"address": "A4:83:E7:2B:19:C0", "active": True, "connected": True},
        {"address": "F0:18:98:6D:42:A7", "active": False, "connected": False},
    ]},
    "auth": {"password": False, "protect_view": False, "logged_in": True},
}

LOG = """board: Pico 2 W, 200 MHz, firmware 9682a73
settings: using saved settings
settings: screen 'laptop' 1920x1200, bluetooth name 'Deskflow Bridge'
web: settings page at http://192.168.77.1/
cyw43 up
ble: restoring pairing with 2 laptops
ble: laptop 1 is active
dhcp: leased 192.168.77.2 to the host
ble: advertising as 'Deskflow Bridge'
ble: connected
ble: link encrypted, security EncryptedAuthenticated
ble: connection interval 15000 us, latency 0
deskflow: client fingerprint 3A:9C:41:07:E2:5B:D8:16:70:AF:C3:2E:94:6D:0B:F1:58:A7:1C:E4:82:3D:96:4F:B0:25:7A:CE:61:D3:19:8E
deskflow: connecting to 192.168.77.2:24800
deskflow: TLS handshake took 1310 ms
deskflow: server speaks protocol 1.8
deskflow: entered the screen
clipboard: 182 characters from the server
"""


class Mock(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send(self, body, kind, extra=()):
        data = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        path, _, query = self.path.partition("?")
        if path == "/":
            self.send(PAGE.read_text(), "text/html; charset=utf-8")
        elif path == "/api/state":
            self.send(json.dumps(STATE), "application/json")
        elif path == "/api/log":
            since = int(query.partition("=")[2] or 0)
            self.send(LOG[since:], "text/plain", [("X-Log-Next", str(len(LOG)))])
        elif path == "/api/panel":
            self.send('{"leds":[true,true,false,false],"hold_ms":[1000,1000,5000]}', "application/json")
        else:
            self.send_error(404)


def main():
    from playwright.sync_api import sync_playwright

    server = ThreadingHTTPServer(("127.0.0.1", 0), Mock)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_port}/"
    OUT.mkdir(parents=True, exist_ok=True)

    with sync_playwright() as p:
        # Containers (and CI) usually can't use Chromium's sandbox.
        browser = p.chromium.launch(args=["--no-sandbox"] if os.environ.get("NO_SANDBOX") else [])
        def section(page, title):
            return page.locator("section", has=page.locator(f"h2:text-is('{title}')")).first

        def band(page, first, last, path):
            top = section(page, first).bounding_box()
            bottom = section(page, last).bounding_box()
            page.screenshot(path=path, full_page=True, clip={
                "x": 0, "y": top["y"] - 16, "width": 900,
                "height": bottom["y"] + bottom["height"] - top["y"] + 32})

        for scheme in ("light", "dark"):
            page = browser.new_page(viewport={"width": 900, "height": 1000}, device_scale_factor=2, color_scheme=scheme)
            page.goto(url)
            page.wait_for_selector("#df:has-text('Connected')")
            page.click("#panel summary")
            page.wait_for_timeout(600)
            if scheme == "light":
                band(page, "Status", "Diagnostics", OUT / "status.png")
                band(page, "Clipboard", "Firmware", OUT / "settings.png")
            else:
                band(page, "Status", "Laptops", OUT / "dark.png")
            page.close()
        phone = browser.new_page(viewport={"width": 390, "height": 844}, device_scale_factor=2)
        phone.goto(url)
        phone.wait_for_selector("#df:has-text('Connected')")
        phone.wait_for_timeout(600)
        phone.screenshot(path=OUT / "phone.png")
        browser.close()
    server.shutdown()
    print("wrote", *sorted(p.name for p in OUT.glob("*.png")))


if __name__ == "__main__":
    main()
