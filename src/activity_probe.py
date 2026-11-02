# Run by omux, locally or on an SSH host, with python3:
#   python3 - snapshot <session-id>     subagents and background shells, as JSON
#   python3 - tail <path> <offset>      a JSON header line, then the new bytes
import glob
import json
import os
import re
import sys
import time

LIMIT = 256 * 1024
now = time.time()


def mtime(path):
    try:
        return os.path.getmtime(path)
    except OSError:
        return 0


def summary(name, given):
    given = given if isinstance(given, dict) else {}
    for key in ("command", "file_path", "pattern", "url", "query", "description", "prompt"):
        value = given.get(key)
        if isinstance(value, str) and value:
            value = " ".join(value.split())
            return f"{name} {value[:90]}"
    return name


def last_tool(path):
    """The last tool an agent used, and how many it used in its last 256 KB."""
    tool, count = None, 0
    try:
        with open(path, "rb") as f:
            f.seek(max(0, os.path.getsize(path) - LIMIT))
            for raw in f:
                if b'"tool_use"' not in raw:
                    continue
                try:
                    row = json.loads(raw)
                except ValueError:
                    continue
                for part in (row.get("message") or {}).get("content") or []:
                    if isinstance(part, dict) and part.get("type") == "tool_use":
                        count += 1
                        tool = summary(part.get("name", "Tool"), part.get("input"))
    except OSError:
        pass
    return tool, count


def snapshot(session):
    base = os.environ.get("CLAUDE_CONFIG_DIR") or os.path.expanduser("~/.claude")
    hits = glob.glob(os.path.join(base, "projects", "*", session + ".jsonl"))
    result = {"agents": [], "shells": []}
    if not hits:
        return result
    path = max(hits, key=mtime)
    session_active = now - mtime(path) < 6 * 3600

    for meta_path in glob.glob(os.path.join(path[:-6], "subagents", "*.meta.json")):
        transcript = meta_path[: -len(".meta.json")] + ".jsonl"
        try:
            meta = json.load(open(meta_path))
        except (OSError, ValueError):
            continue
        finished = bool(meta.get("finished"))
        changed = max(mtime(meta_path), mtime(transcript))
        if finished and now - changed > 600:
            continue
        if not finished and now - changed > 1800:
            continue
        tool, count = last_tool(transcript)
        result["agents"].append({
            "id": os.path.basename(transcript)[:-6],
            "kind": meta.get("agentType") or "agent",
            "description": meta.get("description") or "",
            "finished": finished,
            "background": meta.get("requestShape") == "background",
            "activity": tool,
            "tools": count,
            "path": transcript,
            "modified": changed,
        })

    uses, tasks, status = {}, {}, {}
    try:
        size = os.path.getsize(path)
        with open(path, "rb") as f:
            if size > 8 << 20:
                f.seek(size - (8 << 20))
                f.readline()
            for raw in f:
                if b"run_in_background" not in raw and b"backgroundTaskId" not in raw \
                        and b"<task-id>" not in raw:
                    continue
                text = raw.decode("utf-8", "replace")
                for m in re.finditer(r"<task-id>(\w+)</task-id>.*?<status>(\w+)</status>", text, re.S):
                    status[m.group(1)] = m.group(2)
                try:
                    row = json.loads(raw)
                except ValueError:
                    continue
                content = (row.get("message") or {}).get("content")
                if not isinstance(content, list):
                    continue
                for part in content:
                    if not isinstance(part, dict):
                        continue
                    given = part.get("input") or {}
                    if part.get("type") == "tool_use" and isinstance(given, dict) \
                            and given.get("run_in_background"):
                        uses[part.get("id")] = (given.get("command") or "", given.get("description") or "")
                    elif part.get("type") == "tool_result" and part.get("tool_use_id") in uses:
                        said = part.get("content")
                        said = said if isinstance(said, str) else json.dumps(said)
                        extra = row.get("toolUseResult")
                        task = extra.get("backgroundTaskId") if isinstance(extra, dict) else None
                        if not task:
                            m = re.search(r"with ID: (\w+)", said)
                            task = m.group(1) if m else None
                        m = re.search(r"written to: (\S+?\.output)", said)
                        if task:
                            tasks[task] = (part["tool_use_id"], m.group(1) if m else "")
    except OSError:
        pass
    for task, (use, output) in tasks.items():
        command, description = uses[use]
        state = status.get(task, "running")
        changed = mtime(output)
        if state == "running" and not session_active:
            continue
        if state != "running" and now - changed > 900:
            continue
        result["shells"].append({
            "id": task,
            "command": command,
            "description": description,
            "status": state,
            "output": output,
            "modified": changed,
        })
    return result


def tail(path, offset):
    try:
        size = os.path.getsize(path)
    except OSError:
        size = 0
    start = offset
    if offset > size:
        start = 0
    if start == 0 and size > LIMIT:
        start = size - LIMIT
    data = b""
    if size > start:
        with open(path, "rb") as f:
            f.seek(start)
            data = f.read(LIMIT)
    sys.stdout.write(json.dumps({"start": start, "size": size}) + "\n")
    sys.stdout.flush()
    sys.stdout.buffer.write(data)


if sys.argv[1] == "snapshot":
    print(json.dumps(snapshot(sys.argv[2])))
else:
    tail(sys.argv[2], int(sys.argv[3]))
