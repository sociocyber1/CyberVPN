#!/usr/bin/env python3
"""
Mark busy keys in the free server list, for CyberVPN.

Runs on the VPN server (cron, e.g. every 5 minutes). It asks the 3x-ui panel
which clients are online right now, then rewrites the list in a local clone
of github.com/sociocyber1/txt: every hy2:// line whose client is online gets
"  # busy" after the link, every other line loses the mark. CyberVPN reads
these marks, shows "busy" in the server list and picks a free key.
Changes are committed and pushed only when something changed.

The panel password stays on the server; the apps only read the public list.

Setup (on the server):
    git clone git@github.com:sociocyber1/txt.git /opt/txt     # with push access
    export XUI_URL=http://127.0.0.1:2053/your-panel-path      # panel address
    export XUI_USER=admin XUI_PASS=...                        # panel login
    export TXT_REPO=/opt/txt
    python3 xui-status.py           # once, to test
    # cron:  */5 * * * *  . /etc/xui-status.env && /usr/bin/python3 /opt/xui-status.py

Matching: 3x-ui share links are named "<remark>-<client email>", so a line is
busy when an online client's email appears in the link's name (after #).
"""

import http.cookiejar
import json
import os
import re
import subprocess
import sys
import urllib.parse
import urllib.request

XUI_URL = os.environ.get("XUI_URL", "http://127.0.0.1:2053").rstrip("/")
XUI_USER = os.environ.get("XUI_USER", "")
XUI_PASS = os.environ.get("XUI_PASS", "")
REPO = os.environ.get("TXT_REPO", "/opt/txt")
LIST = os.path.join(REPO, os.environ.get("TXT_FILE", "README.md"))
MARK = "  # busy"


def online_clients():
    jar = http.cookiejar.CookieJar()
    op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    data = urllib.parse.urlencode({"username": XUI_USER, "password": XUI_PASS}).encode()
    r = json.load(op.open(XUI_URL + "/login", data, timeout=15))
    if not r.get("success"):
        sys.exit("3x-ui login failed: " + str(r.get("msg")))
    r = json.load(op.open(urllib.request.Request(XUI_URL + "/panel/api/inbounds/onlines", data=b"", method="POST"), timeout=15))
    return [e for e in (r.get("obj") or []) if e]


def main():
    subprocess.run(["git", "-C", REPO, "pull", "-q", "--rebase"], check=True)  # your new keys first
    online = online_clients()
    with open(LIST, encoding="utf-8") as f:
        lines = f.read().split("\n")
    out = []
    for line in lines:
        m = re.search(r"(hy2|hysteria2)://\S+", line)
        if not m:
            out.append(line)
            continue
        link = m.group(0)
        name = urllib.parse.unquote(link.split("#", 1)[1]) if "#" in link else ""
        busy = any(e in name for e in online)
        base = line[: m.end()]
        out.append(base + MARK if busy else base)
    new = "\n".join(out)
    if new == "\n".join(lines):
        print(f"no change ({len(online)} online)")
        return
    with open(LIST, "w", encoding="utf-8") as f:
        f.write(new)
    subprocess.run(["git", "-C", REPO, "commit", "-qam", f"status: {len(online)} keys busy"], check=True)
    subprocess.run(["git", "-C", REPO, "push", "-q"], check=True)
    print(f"updated: {len(online)} busy")


if __name__ == "__main__":
    main()
