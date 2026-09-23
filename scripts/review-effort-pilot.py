#!/usr/bin/env python3
"""Prepare the three local pilot cases. Never invokes a model or reads credentials.

Usage: python3 scripts/review-effort-pilot.py NEW_ABSOLUTE_DIRECTORY
Build/run examples/review_effort_pilot.rs separately with the emitted run-env.json.
Answer keys stay outside each fixture repository and review export.
"""
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys

IMAGE = "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d"
GOOD = 'authorize() { [ "$1" = correct-credential ]; }\n'
BAD = 'authorize() { [ -n "$1" ]; }\n'
B_CHECK = '''set -eu
. ./authorization.sh
receipt() { if [ -n "${KRANZ_CONTROL_RESULT:-}" ]; then printf '%s' "$1" > "$KRANZ_CONTROL_RESULT"; fi; }
state=0
mutate() { if authorize "$1"; then state=1; fi; }
mutate ''
[ "$state" = 0 ] || exit 5
mutate wrong-nonempty-credential
if [ "$state" != 0 ]; then
  receipt '{"checksRun":2,"outcome":"failed","failureId":"invalid-credential-authorized"}'
  exit 1
fi
mutate correct-credential
[ "$state" = 1 ] || exit 6
receipt '{"checksRun":3,"outcome":"passed"}'
printf '3 assertions passed\n'
'''
LEGACY = '''def parse(text):
    result = {}
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        key, sep, value = line.partition('=')
        if sep:
            result[key.strip().lower()] = value.strip()
    return result
'''
NEW = '''def parse(text):
    entries = (line.partition('=') for line in text.splitlines()
               if line.strip() and not line.lstrip().startswith('#'))
    return {key.strip().lower(): value.strip()
            for key, separator, value in entries if separator}
'''
Y_CHECK = '''import importlib.util
import json
import os
from pathlib import Path
module = 'replacement' if Path('replacement.py').exists() else 'legacy'
spec = importlib.util.spec_from_file_location(module, module + '.py')
parser = importlib.util.module_from_spec(spec)
spec.loader.exec_module(parser)
cases = json.loads(Path('corpus.json').read_text())
for count, case in enumerate(cases, 1):
    if parser.parse(case['input']) != case['expected']:
        receipt = dict(checksRun=count, outcome='failed', failureId='parser-parity')
        if os.environ.get('KRANZ_CONTROL_RESULT'):
            Path(os.environ['KRANZ_CONTROL_RESULT']).write_text(json.dumps(receipt))
        raise SystemExit(1)
receipt = dict(checksRun=len(cases), outcome='passed')
if os.environ.get('KRANZ_CONTROL_RESULT'):
    Path(os.environ['KRANZ_CONTROL_RESULT']).write_text(json.dumps(receipt))
print(str(len(cases)) + ' parity assertions passed against ' + module)
'''
CHECKER = '''import hashlib, json, socket, sys
from pathlib import Path
request = json.load(sys.stdin)
p = request['params']
digest = lambda b: 'sha256:' + hashlib.sha256(b).hexdigest()
manifest_bytes = Path(p['evidence']['path']).read_bytes()
assert digest(manifest_bytes) == p['evidence']['digest']
manifest = json.loads(manifest_bytes)
inventory = []
for artifact in manifest['artifacts']:
    content = artifact['content']
    data = Path(content['path']).read_bytes()
    assert digest(data) == content['digest']
    inventory.append(dict(path=content['path'], digest=digest(data), bytes=len(data)))
denials = {}
for name, action in [
    ('input-write', lambda: Path(p['evidence']['path']).write_text('tampered')),
    ('checker-write', lambda: Path(__file__).write_text('tampered')),
    ('root-write', lambda: Path('/outside-output').write_text('tampered')),
    ('network', lambda: socket.create_connection(('1.1.1.1', 443), timeout=.1)),
]:
    try:
        action()
        denials[name] = False
    except OSError:
        denials[name] = True
assert all(denials.values()), denials
artifact_bytes = json.dumps(dict(inputs=inventory, denials=denials), sort_keys=True).encode()
Path('/gate/outputs/input-check.json').write_bytes(artifact_bytes)
result = dict(schemaVersion=1, evaluationId=p['evaluationId'], attemptId=p['attemptId'],
    binding=p['binding'], evidenceDigest=p['evidence']['digest'], status='judged', verdict='pass',
    rationale='Mechanical input hashes and containment probes passed; no behavioral or human judgment.',
    artifacts=[dict(path='input-check.json', digest=digest(artifact_bytes), bytes=len(artifact_bytes))])
print(json.dumps(dict(jsonrpc='2.0', id=request['id'], result=result)))
'''


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def git(root, *args):
    return subprocess.check_output(['git', '-c', 'commit.gpgsign=false',
        '-c', 'core.hooksPath=/dev/null', *args], cwd=root, env=GIT_ENV, text=True).strip()


def files(root, values):
    for name, value in values.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value)


def control(checker, valid, defective, failure, baseline, baseline_expected):
    as_files = lambda v: [dict(path=p, content=c) for p, c in v.items()]
    return dict(checkerFiles=as_files(checker), validFiles=as_files(valid),
        defectiveFiles=as_files(defective), expectedFailure=failure, timeoutSeconds=15,
        baselinePair=dict(baselineRevision=baseline, expectedBaseline=baseline_expected,
            expectedCandidate=dict(outcome='passed'), environmentLabel='local-pilot-fs+net',
            overlayCheckerOnBaseline=True))


def prepare(root, case):
    folder = root / case
    primary = folder / 'primary'
    primary.mkdir(parents=True)
    git(primary, 'init', '--template=', '-q', '-b', 'main')
    git(primary, 'config', 'user.name', 'Pilot Fixture')
    git(primary, 'config', 'user.email', 'fixture@example.invalid')
    git(primary, 'config', 'commit.gpgsign', 'false')
    common = {'.gitignore': '__pycache__/\n', 'pack/checker.py': CHECKER,
        'pack/pack.toml': f"""[pack]
name='review-pilot-input-check'
schema=5
[[evaluator]]
name='review-pilot-input-check'
image='{IMAGE}'
executable='/usr/local/bin/python3'
args=['-I','-S','/checker/checker.py']
files=['checker.py']
stages=['plan-approval','milestone-validation','final-gate']
evidence=['scope','check-receipt']
kind='mechanical'
enforcement='blocking'
"""}
    if case == 'D1':
        initial = {'README.md': '# Fixture CLI\n\nUse `sh cli.sh` to inspect status.\n',
            'cli.sh': "printf 'local status only\\n'\n"}
        writes = {'README.md': '# Fixture CLI\n\n`sh cli.sh` prints local status. It does not contact a remote service.\n'}
        later = {'README.md': writes['README.md'] + '\nThe command also publishes your changes remotely.\n'}
        check_files = {'check.sh': "set -eu\n[ \"$(sh cli.sh)\" = 'local status only' ]\n! grep -q 'publishes your changes remotely' README.md\nprintf '2 assertions passed\\n'\n"}
        goal = 'Clarify the fixture CLI local-only behavior without changing executable behavior.'
        command = 'sh check.sh'
        answer = 'Request repair: README claims remote publishing unsupported by cli.sh, and the pass precedes that edit. Rerun current checks after correction. No baseline pair applies to this docs-only case.'
    elif case == 'B1':
        initial = {'authorization.sh': BAD, 'README.md': '# Synthetic authorization fixture\nOnly correct-credential may mutate state. No real users or services.\n'}
        writes = {'authorization.sh': GOOD}
        later = {}
        check_files = {'check.sh': B_CHECK}
        goal = 'Reject a wrong nonempty synthetic credential while allowing the exact correct credential.'
        command = 'sh check.sh'
        answer = 'Behavioral evidence must name invalid-credential-authorized on the actual baseline and pass the candidate, with valid/defective controls. The separate missing-dependency variant is inconclusive, never reproduction. Accept only evidence actually present.'
    else:
        initial = {'legacy.py': LEGACY, 'app.py': 'from legacy import parse\n\ndef run(text):\n    return parse(text)\n',
            'README.md': '# Parser migration\nPreserve comments, trimming, lowercase keys, duplicate-last-wins, and embedded equals. Finish by routing app.run through the replacement and deleting legacy.py.\n'}
        writes = {'replacement.py': NEW}
        later = {}
        corpus = [dict(input='', expected={}), dict(input=' # comment\nNo separator\n', expected={}),
            dict(input=' A = first\na=last ', expected={'a':'last'}),
            dict(input='value=a=b\nblank=\n', expected={'value':'a=b','blank':''}),
            dict(input=' Mixed KEY = Keep Case\n', expected={'mixed key':'Keep Case'})]
        check_files = {'check.py': Y_CHECK, 'corpus.json': json.dumps(corpus, indent=2) + '\n'}
        goal = 'Replace the legacy parser, preserve its characterized behavior, route app.run to the replacement, and remove the legacy implementation.'
        command = f'{shlex.quote(sys.executable)} -B check.py'
        answer = 'Request repair: new parser passes parity, but app.run still imports legacy and legacy.py remains. No cutover has occurred. Parity is necessary but insufficient; separately verify entrypoint routing and removal.'
    files(primary, common | initial)
    git(primary, 'add', '.')
    git(primary, 'commit', '-qm', 'synthetic pilot source baseline')
    files(primary, check_files)
    git(primary, 'add', *check_files)
    git(primary, 'commit', '-qm', 'freeze checks and characterization before candidate')
    baseline = git(primary, 'rev-parse', 'HEAD')
    assertion = dict(id='a-1', statement=goal, check='command', command=command)
    if case == 'B1':
        assertion['negativeControl'] = control(check_files, {'authorization.sh':GOOD},
            {'authorization.sh':BAD}, 'invalid-credential-authorized', baseline,
            dict(outcome='failed', failureId='invalid-credential-authorized'))
    if case == 'Y1':
        assertion['negativeControl'] = control(check_files, {'replacement.py':NEW},
            {'replacement.py':'def parse(text):\n    return {}\n'}, 'parser-parity', baseline,
            dict(outcome='passed'))
    touch = list(writes) if case != 'Y1' else ['replacement.py', 'app.py', 'legacy.py']
    plan = dict(goal=goal, touchSet=touch, validationContract=[assertion],
        milestones=[dict(title=case + ' candidate', features=[dict(title=goal,
            spec='Apply the frozen synthetic candidate. No provider invocation. ' + goal,
            validationCriteria=[goal])])])
    report = dict(result='pass', summary='Scripted fixture candidate; human review pending.',
        filesTouched=list(writes), testsAdded=[], dependenciesAdded=[], knownGaps=[],
        commits=[], commandsRun=[], escalation=None, questions=[])
    marker = 'PILOT-WORKER-TRANSCRIPT-' + hashlib.sha256((str(folder) + case).encode()).hexdigest()
    prepared = dict(case=case, baseline=baseline, plan=plan, report=report, writes=writes,
        laterEdits=later, transcriptMarker=marker, reviewer='Craig Martin',
        priorSeedKnowledge='pending reviewer declaration; protocol publicly describes seeds',
        agentExecution='scripted fixture only; no model judgment', providerCalls=0)
    save(folder / 'prepared.json', prepared)
    save(root / 'answer-keys' / (case + '.json'), dict(case=case, expectedResponse=answer))


if __name__ == '__main__':
    root = Path(sys.argv[1])
    if not root.is_absolute() or root.exists():
        raise SystemExit('a new absolute directory is required')
    os.umask(0o077)
    root.mkdir(parents=True)
    (root / 'answer-keys').mkdir()
    home = root / 'empty-home'
    (home / '.claude').mkdir(parents=True)
    docker = root / 'docker-config'
    docker.mkdir()
    save(docker / 'config.json', {'auths': {}})
    GIT_ENV = dict(PATH=os.environ['PATH'], HOME=str(home), GIT_CONFIG_NOSYSTEM='1',
        GIT_CONFIG_GLOBAL='/dev/null')
    for case in ('D1', 'B1', 'Y1'):
        prepare(root, case)
    env = GIT_ENV | dict(CLAUDE_CONFIG_DIR=str(home / '.claude'),
        DOCKER_CONFIG=str(docker),
        KRANZ_SCRATCH_ROOT=str(root / 'scratch'), TMPDIR=str(root / 'scratch'), KRANZ_SGIAN_BIN='')
    (root / 'scratch').mkdir()
    if os.environ.get('DOCKER_HOST'):
        env['DOCKER_HOST'] = os.environ['DOCKER_HOST']
    save(root / 'run-env.json', env)
    paths = [p for p in root.rglob('*') if p.is_file() and '.git' not in p.parts]
    save(root / 'preparation-inventory.json', [dict(path=str(p.relative_to(root)),
        bytes=p.stat().st_size, sha256=hashlib.sha256(p.read_bytes()).hexdigest()) for p in sorted(paths)])
    print(root)
