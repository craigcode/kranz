#!/usr/bin/env python3
"""Real-container, synthetic-peer qualification of shell-once. Never uses a vendor."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1]).resolve())
image = sys.argv[2]
root = Path(tempfile.mkdtemp(prefix="kranz-acp-tool-proof-"))
peer = r'''
import errno, json, os, subprocess, sys
from pathlib import Path
mode = sys.argv[1]
assert not any(k in os.environ for k in ['ANTHROPIC_API_KEY','CLAUDE_CODE_OAUTH_TOKEN','CODEX_API_KEY','OPENAI_API_KEY','GH_TOKEN','NODE_OPTIONS'])
command = 'echo kranz-acp-tool-fixture-v1 > fixture-result.txt'
tool = {'toolCallId':'shell-1', 'kind':'execute', 'title':'Fixture shell', 'status':'pending', 'rawInput':{'command':command,'cwd':str(Path.cwd())}}
options = [{'optionId':'opaque-yes','kind':'allow_once','name':'Once'}, {'optionId':'opaque-no','kind':'reject_once','name':'No'}]
def send(value):
 print(json.dumps(dict(jsonrpc='2.0', **value)), flush=True)
def update(value):
 send({'method':'session/update','params':{'sessionId':'fixture-peer','update':value}})
def request(number=10):
 send({'id':number,'method':'session/request_permission','params':{'sessionId':'fixture-peer','toolCall':tool,'options':options}})
def deliver():
 if mode == 'wrong-output': Path('fixture-result.txt').write_text('wrong\n')
 elif mode == 'symlink': Path('fixture-result.txt').symlink_to('README')
 else: subprocess.run(['/bin/sh','-c',command],check=True)
 if mode == 'extra-file': Path('extra').write_text('unauthorized')
 if mode == 'primary-change':
  # A protected file must not be writable through the shared gitdir.
  gitdir=Path('.git').read_text().strip().split(': ',1)[1]
  config=Path(gitdir).parent.parent/'config'
  config=Path(os.path.normpath(str(config)))
  assert config == Path.cwd().parent/'primary/.git/config', 'probe must target the host-verified primary config'
  # This fixture intentionally does not expose the shared Git directory.
  # The host verifies that this exact target exists and remains unchanged.
  try: config.write_text('[core]\n bare=true\n')
  except OSError as error: assert error.errno in (errno.ENOENT, errno.EACCES, errno.EPERM, errno.EROFS), error
  else: raise AssertionError('protected primary config was writable')
 status = 'failed' if mode == 'tool-failed' else 'completed'
 raw_output={'exit_code':1 if mode == 'nonzero-exit' else 0}
 update({'sessionUpdate':'tool_call_update','toolCallId':'shell-1','status':status,'rawOutput':raw_output})
 update({'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':json.dumps(report)}})
 send({'id':prompt_id,'result':{'stopReason':'end_turn'}})
for line in sys.stdin:
 r=json.loads(line); method=r.get('method'); ident=r.get('id')
 if method=='initialize': send({'id':ident,'result':{'protocolVersion':1,'agentInfo':{'name':'synthetic-shell-probe','version':'1'}}})
 elif method=='session/new': send({'id':ident,'result':{'sessionId':'fixture-peer'}})
 elif method=='session/prompt':
  prompt_id=ident
  report=json.loads(r['params']['prompt'][0]['text'].rsplit('\n',1)[1])
  if mode == 'wrong-command': tool['rawInput']['command'] += '; echo extra'
  if mode == 'wrong-cwd': tool['rawInput']['cwd']='/tmp'
  if mode == 'extra-authority': tool['rawInput']['additionalPermissions']={'network':True}
  if mode == 'ambiguous-once': options.append({'optionId':'second-yes','kind':'allow_once','name':'Another once'})
  if mode == 'durable-only': options[0]['kind']='allow_always'
  if mode == 'option-extension': options[0]['_meta']={'durable':True}
  if mode == 'early-write': Path('fixture-result.txt').write_text('early\n')
  if mode == 'no-permission':
   update(dict(tool,sessionUpdate='tool_call'))
   deliver()
  elif mode == 'report-only':
   update({'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':json.dumps(report)}})
   send({'id':ident,'result':{'stopReason':'end_turn'}})
  else:
   announced=dict(tool,sessionUpdate='tool_call')
   if mode == 'wrapped-command': announced['rawInput']={'command':"/bin/bash -lc '"+command+"'",'cwd':str(Path.cwd())}
   if mode == 'streamed-input': announced['rawInput']={}
   update(announced)
   if mode == 'permission-command': tool['rawInput']['command'] += '; echo extra'
   if mode == 'permission-cwd': tool['rawInput']['cwd']='/tmp'
   request()
 elif method=='session/cancel': break
 elif method is None:
  if ident == 10:
   assert r.get('result',{}).get('outcome',{}).get('optionId') == 'opaque-yes', r
   if mode == 'repeat-request': request(11)
   elif mode == 'drift':
    update({'sessionUpdate':'tool_call_update','toolCallId':'shell-1','rawInput':{'command':command+'; echo changed'}})
   elif mode == 'wrong-tool-id':
    update({'sessionUpdate':'tool_call_update','toolCallId':'shell-2','status':'completed'})
   else: deliver()
 else: raise AssertionError(method)
'''
(root / "peer.py").write_text(peer)
cases = ["pass", "wrapped-command", "streamed-input", "primary-change", "permission-command", "permission-cwd", "wrong-command", "wrong-cwd", "extra-authority", "ambiguous-once", "durable-only", "option-extension", "early-write", "no-permission", "report-only", "wrong-output", "symlink", "extra-file", "tool-failed", "nonzero-exit", "repeat-request", "drift", "wrong-tool-id", "default-report"]
errors = {
    "default-report": "tool activity invalidates this no-tools fixture",
    "wrong-command": "command differs", "permission-command": "command differs",
    "wrong-cwd": "unsupported tool input field: cwd", "permission-cwd": "unsupported tool input field: cwd",
    "extra-authority": "unsupported tool input field: additionalPermissions",
    "ambiguous-once": "permission is repeated, prohibited", "durable-only": "permission is repeated, prohibited",
    "option-extension": "unsupported extensions", "early-write": "unexpected paths",
    "no-permission": "after a delivered one-time grant", "report-only": "missing tool/permission/completion evidence",
    "wrong-output": "fixture output differs", "symlink": "fixture file is not regular", "extra-file": "unexpected paths",
    "tool-failed": "after a delivered one-time grant", "nonzero-exit": "tool output reports failure",
    "repeat-request": "permission is repeated, prohibited", "drift": "command differs", "wrong-tool-id": "more than one tool invocation",
}
invalid = root / "uncontained.json"
invalid.write_text(json.dumps(dict(provider="fixture",mode="shell-once",program=sys.executable,args=["-c","raise AssertionError('must not spawn')"],receipt=str(root/"uncontained.jsonl"))))
check = subprocess.run([binary,"--check",str(invalid)], capture_output=True,text=True,timeout=10)
assert check.returncode != 0 and "shell-once requires an enforced pinned container" in check.stderr
assert not (root / "uncontained.jsonl").exists()
results = []
def inventory():
    out = subprocess.run(["docker", "container", "ls", "--all", "--no-trunc", "--format", "{{.ID}}", "--filter", "label=com.kranz.acp-owner"], capture_output=True, text=True, timeout=10, check=True)
    return set(out.stdout.splitlines())

print(root, flush=True)
for mode in cases:
    receipt = root / f"{mode}.jsonl"
    config = dict(provider="fixture", mode="shell-once", program="/usr/local/bin/python3", args=["-c", peer, mode], container=dict(image=image, egress=[]), receipt=str(receipt))
    if mode == "default-report": del config["mode"]
    path = root / f"{mode}.json"
    path.write_text(json.dumps(config))
    check = subprocess.run([binary, "--check", str(path)], capture_output=True, text=True, timeout=10)
    assert check.returncode == 0, check.stderr
    preview = json.loads(check.stdout)
    assert preview["permissionLimit"] == (0 if mode == "default-report" else 1)
    assert preview["mode"] == ("report-only" if mode == "default-report" else "shell-once")
    assert not preview["adapterStarted"] and not receipt.exists()
    before = inventory()
    run = subprocess.run([binary, "--run", str(path)], capture_output=True, text=True, timeout=240)
    deadline = time.monotonic() + 15
    while inventory() - before:
        assert time.monotonic() < deadline, f"{mode}: container cleanup unconfirmed"
        time.sleep(0.1)
    rows = [json.loads(line) for line in receipt.read_text().splitlines()]
    expected = mode in ("pass", "wrapped-command", "streamed-input", "primary-change")
    assert (run.returncode == 0) == expected, (mode, run.returncode, run.stderr, rows[-1])
    assert rows[-1]["event"] == "probe.finished" and rows[-1]["passed"] == expected
    assert rows[-1]["toolFixtureProven"] == expected and not rows[-1]["productionReadinessProven"]
    if expected:
        order = [row["event"] for row in rows]
        assert order.index("permission.requested") < order.index("permission.resolved") < order.index("permission.responded") < order.index("probe.tool.deliverable")
        assert sum(e == "permission.resolved" for e in order) == 1
        delivered = next(row for row in rows if row["event"] == "probe.tool.deliverable")
        assert delivered["featureCommits"] == 1 and delivered["primaryUnchanged"]
        assert delivered["commit"] != delivered["base"] and not delivered["missionCompletionProven"]
    else:
        assert errors[mode] in rows[-1]["error"], (mode, rows[-1])
        assert not any(row["event"] == "probe.tool.deliverable" for row in rows)
    results.append(dict(case=mode, expectedPass=expected, exit=run.returncode, error=rows[-1]["error"]))
    (root / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    print(f"PASS: {mode} ({'accepted' if expected else 'refused'})", flush=True)
print(f"PASS: {len(cases)} contained synthetic tool cases; no vendor, credentials or paid calls.", flush=True)
