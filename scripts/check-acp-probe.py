#!/usr/bin/env python3
"""Exercise the bounded ACP probe with a deterministic local peer, never a vendor."""
import json, os, pathlib, subprocess, tempfile, sys
root=pathlib.Path(tempfile.mkdtemp(prefix='kranz-acp-probe-proof-'))
(root/'peer.py').write_text('''import json,os,sys
from pathlib import Path
assert 'KRANZ_PROBE_UNRELATED_SECRET' not in os.environ
assert not any(k in os.environ for k in ['ANTHROPIC_API_KEY','OPENAI_API_KEY','CODEX_API_KEY','GH_TOKEN','NODE_OPTIONS'])
assert Path(os.environ['HOME']).name == 'home'
assert Path(os.environ['HOME']).parent != Path.cwd()
Path(sys.argv[1]).write_text('started')
for line in sys.stdin:
 r=json.loads(line); method=r.get('method'); ident=r.get('id')
 if method=='initialize': result={'protocolVersion':1,'agentInfo':{'name':'deterministic-probe-fixture','version':'1'}}
 elif method=='session/new': result={'sessionId':'fixture-peer','configOptions':[{'id':'model','category':'model','currentValue':'fixture-model'}]}
 elif method=='session/prompt':
  text=r['params']['prompt'][0]['text']; report=json.loads(text.split('\\n',1)[1])
  print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'fixture-peer','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':json.dumps(report)}}}}),flush=True)
  result={'stopReason':'end_turn'}
 else: raise AssertionError(method)
 print(json.dumps({'jsonrpc':'2.0','id':ident,'result':result}),flush=True)
''')
config={'provider':'fixture','program':sys.executable,'args':[str(root/'peer.py'),str(root/'started')],'credentialEnv':None,'receipt':str(root/'receipt.jsonl')}
(root/'config.json').write_text(json.dumps(config))
binary=str(pathlib.Path(sys.argv[1]).resolve()) if len(sys.argv) == 2 else str(pathlib.Path('target/debug/examples/acp_compat_probe').resolve())
env=dict(os.environ,KRANZ_PROBE_UNRELATED_SECRET='test')
check=subprocess.run([binary,'--check',str(root/'config.json')],capture_output=True,text=True,env=env,timeout=5)
assert check.returncode==0,(check.returncode,check.stderr)
assert not (root/'started').exists() and not (root/'receipt.jsonl').exists()
run=subprocess.run([binary,'--run',str(root/'config.json')],capture_output=True,text=True,env=env,timeout=10)
assert run.returncode==0,(run.returncode,run.stderr,(root/'receipt.jsonl').read_text())
rows=[json.loads(x) for x in (root/'receipt.jsonl').read_text().splitlines()]
assert rows[-1]['passed'] and not rows[-1]['productionReadinessProven']
assert rows[0]['provider']=='fixture' and 'KRANZ_PROBE_UNRELATED_SECRET' not in rows[0]['environmentKeys']
assert 'fixture-model' in (root/'receipt.jsonl').read_text()
before=(root/'started').stat().st_mtime_ns
repeat=subprocess.run([binary,'--run',str(root/'config.json')],capture_output=True,text=True,env=env,timeout=5)
assert repeat.returncode!=0 and (root/'started').stat().st_mtime_ns==before
(root/'test-receipt.json').write_text(json.dumps({'proof':'deterministic fixture only','checkDoesNotSpawn':True,'actualProbePasses':True,'unrelatedEnvAbsent':True,'repeatRejectedBeforeSpawn':True},indent=2))
print(root)
print('PASS: check does not spawn; fixture report parses; private environment; existing receipt prevents retry.')
