#!/usr/bin/env python3
"""Resolves VS Code's user-level mcp.json path for the default profile.

stdin:  {"mcp_path": "...", "os": "macos"|"linux"|"windows", "home": "..."}
stdout: {"status": "ok", "data": {"path": "..."}} or {"status": "skip", "message": "..."}
"""
import json
import sys

data = json.load(sys.stdin)
os_name = data["os"]
home = data["home"]

paths = {
    "macos": f"{home}/Library/Application Support/Code/User/mcp.json",
    "linux": f"{home}/.config/Code/User/mcp.json",
    "windows": f"{home}/AppData/Roaming/Code/User/mcp.json",
}

path = paths.get(os_name)
if path is None:
    print(json.dumps({"status": "skip", "message": f"unsupported OS: {os_name}"}))
else:
    print(json.dumps({"status": "ok", "data": {"path": path}}))
