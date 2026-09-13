#!/usr/bin/env python3
"""Resolves Zed's settings.json path (default profile) and generates the
context_servers.infigraph fragment directly -- Zed's settings.json is a
general-purpose file with far more content than just this section, so unlike
VS Code there's no separate mirrored file to keep as a static local fragment.

stdin:  {"mcp_path": "...", "os": "macos"|"linux"|"windows", "home": "..."}
stdout: {"status": "ok", "data": {"path": "...", "content": {...}}} or {"status": "skip", "message": "..."}
"""
import json
import sys

data = json.load(sys.stdin)
os_name = data["os"]
home = data["home"]
mcp_path = data["mcp_path"]

paths = {
    "macos": f"{home}/Library/Application Support/Zed/settings.json",
    "linux": f"{home}/.config/zed/settings.json",
    "windows": f"{home}/AppData/Roaming/Zed/settings.json",
}

path = paths.get(os_name)
if path is None:
    print(json.dumps({"status": "skip", "message": f"unsupported OS: {os_name}"}))
else:
    content = {"command": mcp_path, "args": ["--mcp"], "env": {}}
    print(json.dumps({"status": "ok", "data": {"path": path, "content": content}}))
