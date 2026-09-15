"""Synthetic, non-model checker for the external evaluator boundary tests."""
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time

mode = sys.argv[1]
if mode == 'stall-input':
    time.sleep(60)
request = json.load(sys.stdin)
p = request['params']
digest = lambda b: 'sha256:' + hashlib.sha256(b).hexdigest()
manifest_bytes = Path(p['evidence']['path']).read_bytes()
assert digest(manifest_bytes) == p['evidence']['digest']
manifest = json.loads(manifest_bytes)
for artifact in manifest['artifacts']:
    content = artifact['content']
    assert digest(Path(content['path']).read_bytes()) == content['digest']
result = dict(schemaVersion=1, evaluationId=p['evaluationId'], attemptId=p['attemptId'],
              binding=p['binding'], evidenceDigest=p['evidence']['digest'],
              status='judged', verdict='pass', rationale='Synthetic checker completed.', artifacts=[])
response = dict(jsonrpc='2.0', id=request['id'], result=result)

def artifact(path, content):
    Path('/gate/outputs', path).write_bytes(content)
    result['artifacts'].append(dict(path=path, digest=digest(content), bytes=len(content)))

if mode == 'containment':
    assert set(os.environ).issubset({'HOME', 'TMPDIR', 'PATH', 'PWD', 'LC_CTYPE'})
    assert not any('KEY' in key or 'TOKEN' in key for key in os.environ)
    checks = {}
    for name, action in [
        ('authority-read', lambda: Path(sys.argv[2]).read_bytes()),
        ('candidate-write', lambda: Path(sys.argv[3]).write_text('tampered')),
        ('input-write', lambda: Path(p['evidence']['path']).write_text('tampered')),
        ('checker-write', lambda: Path(__file__).write_text('tampered')),
        ('driver-write', lambda: Path('/driver/entry.sh').write_text('tampered')),
        ('root-write', lambda: Path('/host-output').write_text('tampered')),
        ('network', lambda: socket.create_connection(('1.1.1.1', 443), timeout=0.1)),
    ]:
        try:
            action()
            checks[name] = 'ALLOWED'
        except OSError:
            checks[name] = 'DENIED'
    assert all(value == 'DENIED' for value in checks.values()), checks
    artifact('checks.json', json.dumps(checks, sort_keys=True).encode())
if mode == 'redaction':
    secret = 'ghp_' + 'A' * 36
    artifact('redacted.txt', secret.encode())
    result['rationale'] = secret
if mode == 'artifact':
    artifact('evidence.txt', b'A safe result.\n')
if mode == 'finding' or mode == 'bad-line':
    source = next(a for a in manifest['artifacts'] if a['role'] == 'source')
    end = 2 if mode == 'finding' else 3
    result['findings'] = [dict(id='finding-1', severity='high', summary='Synthetic finding.', evidence=[dict(artifactId=source['id'], digest=source['content']['digest'], lineStart=1, lineEnd=end)])]
if mode == 'wrong-binding':
    result['binding']['policyDigest'] = digest(b'unrelated')
if mode == 'wrong-id':
    result['attemptId'] = 'unrelated'
if mode == 'authority':
    result['actor'] = 'human:forged'
if mode == 'version':
    result['schemaVersion'] = 2
if mode == 'escalate':
    result['status'] = 'escalate'
    result.pop('verdict')
if mode == 'fail':
    result['verdict'] = 'fail'
if mode in ('symlink', 'parent-symlink', 'fifo', 'hardlink', 'wrong-hash', 'binary', 'traversal'):
    artifact('result.txt', b'test')
    path = Path('/gate/outputs/result.txt')
    if mode == 'symlink':
        path.unlink()
        path.symlink_to('/gate/inputs/manifest.json')
    if mode == 'parent-symlink':
        Path('/gate/outputs/nested').symlink_to('/gate/inputs', target_is_directory=True)
        result['artifacts'][0]['path'] = 'nested/manifest.json'
    if mode == 'fifo':
        path.unlink()
        os.mkfifo(path)
    if mode == 'hardlink':
        os.link(path, '/gate/outputs/second.txt')
    if mode == 'wrong-hash':
        result['artifacts'][0]['digest'] = digest(b'wrong')
    if mode == 'binary':
        path.write_bytes(b'\xffabc')
        result['artifacts'][0]['digest'] = digest(b'\xffabc')
    if mode == 'traversal':
        result['artifacts'][0]['path'] = '../inputs/manifest.json'
if mode in ('descendant', 'cancel', 'drop'):
    # A new session escapes a host process group but remains inside the
    # evaluator's container PID namespace. It must die with that namespace.
    subprocess.Popen([sys.executable, '-I', '-S', '-c', "import pathlib,time; time.sleep(2); pathlib.Path('/gate/outputs/escaped.txt').write_text('alive'); time.sleep(60)"], start_new_session=True)
    Path('/gate/outputs/started.txt').write_text('ready')
    if mode != 'descendant':
        time.sleep(60)
if mode == 'missing':
    sys.exit(0)
if mode == 'overflow':
    print('x' * 1_048_577)
    sys.exit(0)
if mode == 'stderr-overflow':
    sys.stderr.write('x' * 1_048_577)
    sys.exit(0)
wire = json.dumps(response, separators=(',', ':'))
if mode == 'duplicate-key':
    wire = wire.replace('"jsonrpc":"2.0"', '"jsonrpc":"2.0","jsonrpc":"2.0"')
sys.stdout.write(wire + ('' if mode == 'no-newline' else '\n'))
sys.stdout.flush()
if mode == 'duplicate':
    print(wire)
if mode == 'nonzero':
    sys.exit(7)
