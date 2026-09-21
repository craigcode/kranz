# Deterministic ordinary-worker fixture, compiled only into unit tests.
import json
import os
from pathlib import Path
import subprocess
import sys


def send(value):
    print(json.dumps(value), flush=True)


def reply(request, value):
    send({"jsonrpc": "2.0", "id": request["id"], "result": value})


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        home = Path(os.environ["HOME"])
        try:
            home.parent.chmod(0o777)
        except OSError:
            pass
        else:
            raise AssertionError("private host parent was writable")
        codex = Path(os.environ["CODEX_HOME"])
        assert codex == home / ".codex"
        assert {p.name for p in codex.iterdir()} == {"auth.json", "config.toml"}
        assert 'plugins = false' in (codex / "config.toml").read_text()
        assert 'remote_plugin = false' in (codex / "config.toml").read_text()
        assert 'cli_auth_credentials_store = "file"' in (codex / "config.toml").read_text()
        assert os.environ["INITIAL_AGENT_MODE"] == "read-only"
        assert os.environ["NO_BROWSER"] == "1"
        assert not any(key in os.environ for key in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "NODE_OPTIONS"])
        login_value = json.loads((codex / "auth.json").read_text())["tokens"]["access_token"]
        assert login_value == "synthetic-login-no-authority-12345"
        reply(request, {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": []})
    elif method == "session/new":
        reply(request, {"sessionId": "profile-fixture-session"})
    elif method == "session/prompt":
        if "fixture-expose-login" in str(request):
            Path("source.txt").write_text(str(home))
            print("failure: " + login_value, file=sys.stderr, flush=True)
            send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "profile-fixture-session", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": login_value}}}})
            continue
        command = "printf 'changed\\n' > source.txt"
        if "fixture-seeded-defect" in str(request) and Path("source.txt").read_text() == "base\n":
            command = "printf 'defect\\n' > source.txt"
        send({"jsonrpc": "2.0", "id": "one-action", "method": "session/request_permission", "params": {
            "sessionId": "profile-fixture-session", "toolCall": {"toolCallId": "write-source", "title": "write synthetic deliverable", "kind": "execute", "rawInput": {"command": command}},
            "options": [{"optionId": "once", "name": "Allow once", "kind": "allow_once"}, {"optionId": "deny", "name": "Deny once", "kind": "reject_once"}]}})
        answer = json.loads(sys.stdin.readline())
        assert answer["id"] == "one-action"
        granted = answer["result"]["outcome"].get("optionId") == "once"
        if granted:
            subprocess.run(["/bin/sh", "-c", command], check=True)
        report = {"result": "pass" if granted else "fail", "summary": "synthetic profile worker", "filesTouched": ["source.txt"] if granted else [], "testEvidence": str(home), "commandsRun": [command] if granted else [], "commits": []}
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "profile-fixture-session", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": json.dumps(report)}}}})
        reply(request, {"stopReason": "end_turn"})
