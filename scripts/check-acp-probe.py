#!/usr/bin/env python3
"""Exercise the bounded ACP probe with a deterministic local peer, never a vendor."""
import json, os, pathlib, subprocess, tempfile, sys
root=pathlib.Path(tempfile.mkdtemp(prefix='kranz-acp-probe-proof-'))
(root/'peer.py').write_text('''import json,os,sys
from pathlib import Path
assert 'KRANZ_PROBE_UNRELATED_SECRET' not in os.environ
assert not any(k in os.environ for k in ['ANTHROPIC_API_KEY','CODEX_API_KEY','GH_TOKEN','NODE_OPTIONS'])
assert Path(os.environ['HOME']).name == 'home'
assert Path(os.environ['HOME']).parent != Path.cwd()
if 'CODEX_HOME' in os.environ:
 home=Path(os.environ['CODEX_HOME'])
 assert home == Path(os.environ['HOME'])/'.codex'
 assert (home/'config.toml').read_text() == 'cli_auth_credentials_store = "file"\\n\\n[features]\\nplugins = false\\nremote_plugin = false\\n'
 assert not (home/'history.jsonl').exists()
 if 'OPENAI_API_KEY' in os.environ:
  assert os.environ['OPENAI_API_KEY'] == 'fixture-key-for-config-check'
  assert not (home/'auth.json').exists()
 else:
  assert (home/'auth.json').read_bytes() == b'opaque-fixture-credential'
else:
 assert 'OPENAI_API_KEY' not in os.environ
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
binary=str(pathlib.Path(sys.argv[1]).resolve()) if len(sys.argv) >= 2 else str(pathlib.Path('target/debug/examples/acp_compat_probe').resolve())
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

def check_codex_startup(container=None):
    mode='contained' if container else 'native'
    for channel in ['key', 'login']:
        source=root/f'{mode}-{channel}-source'
        (source/'.codex').mkdir(parents=True)
        (source/'.codex/auth.json').write_bytes(b'opaque-fixture-credential')
        (source/'.codex/config.toml').write_text('operator-settings-must-not-cross')
        (source/'.codex/history.jsonl').write_text('operator-history-must-not-cross')
        before={p.name:p.read_bytes() for p in (source/'.codex').iterdir()}
        started=root/f'{mode}-{channel}-started'
        receipt=root/f'{mode}-{channel}-receipt.jsonl'
        peer=(root/'peer.py').read_text()
        cfg={'provider':'codex','program':sys.executable,'args':['-c',peer,str(started)],'receipt':str(receipt)}
        run_env=dict(env)
        if channel == 'key':
            cfg['credentialEnv']='OPENAI_API_KEY'
            run_env['OPENAI_API_KEY']='fixture-key-for-config-check'
        else:
            cfg['nativeLoginHome']=str(source)
            run_env.pop('OPENAI_API_KEY',None)
        if container:
            peer=peer.replace("Path(sys.argv[1]).write_text('started')", "assert not Path('/kranz-owned-session/lease').stat().st_mode & 0o022")
            cfg.update(program='/usr/local/bin/python3',args=['-c',peer],container={'image':container,'egress':['provider.invalid:443']})
        path=root/f'{mode}-{channel}-config.json'
        path.write_text(json.dumps(cfg))
        check=subprocess.run([binary,'--check',str(path)],capture_output=True,text=True,env=run_env,timeout=5)
        assert check.returncode==0,check.stderr
        preflight=json.loads(check.stdout)
        assert preflight['codexStartupConfig'] and not preflight['codexStartupConfigWritten']
        assert not preflight['adapterStarted'] and not receipt.exists() and not started.exists()
        run=subprocess.run([binary,'--run',str(path)],capture_output=True,text=True,env=run_env,timeout=210 if container else 10)
        assert run.returncode==0,(run.returncode,run.stderr,receipt.read_text() if receipt.exists() else 'no receipt')
        rows=[json.loads(x) for x in receipt.read_text().splitlines()]
        assert rows[0]['codexStartupConfigWritten'] and rows[0]['codexStartupConfig']==preflight['codexStartupConfig']
        assert rows[-1]['passed'] and rows[-1]['containedSession']==bool(container)
        assert not rows[-1]['productionReadinessProven'] and not rows[-1]['containmentProven']
        assert 'CODEX_HOME' in rows[0]['environmentKeys']
        if channel == 'key':
            assert 'fixture-key-for-config-check' not in receipt.read_text()
        assert before=={p.name:p.read_bytes() for p in (source/'.codex').iterdir()}
        if not container:
            assert started.read_text()=='started'
    print(f'PASS: {mode} synthetic Codex key/login peers see plugin policy before initialization; source homes unchanged; no vendor invoked.')

check_codex_startup()
if len(sys.argv) == 3:
    image=sys.argv[2]
    peer=(root/'peer.py').read_text().replace("Path(sys.argv[1]).write_text('started')", "assert not Path('/kranz-owned-session/lease').stat().st_mode & 0o022")
    config.update(program='/usr/local/bin/python3',args=['-c',peer],container={'image':image,'egress':[]},receipt=str(root/'contained-receipt.jsonl'))
    (root/'contained-config.json').write_text(json.dumps(config))
    check=subprocess.run([binary,'--check',str(root/'contained-config.json')],capture_output=True,text=True,env=env,timeout=5)
    assert check.returncode==0 and not (root/'contained-receipt.jsonl').exists(),check.stderr
    run=subprocess.run([binary,'--run',str(root/'contained-config.json')],capture_output=True,text=True,env=env,timeout=210)
    assert run.returncode==0,(run.returncode,run.stderr,(root/'contained-receipt.jsonl').read_text())
    rows=[json.loads(x) for x in (root/'contained-receipt.jsonl').read_text().splitlines()]
    assert rows[-1]['passed'] and rows[-1]['containedSession']
    assert not rows[-1]['containmentProven'] and not rows[-1]['productionReadinessProven']
    init=next(r['raw'] for r in rows if r['event']=='init')
    assert init['containment']['image']==image
    assert init['containment']['providerCompatibilityCertified'] is False
    print('PASS: contained fixture uses the pinned image, parses its report, and confirms cleanup; no vendor invoked.')
    check_codex_startup(image)
