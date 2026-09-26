#!/usr/bin/env python3
"""Check retained adapter locks against proof receipts; offline, no vendor calls."""
import hashlib
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parent.parent / 'docs/compatibility/acp'


def verify(lock_bytes, install, image, native):
    actual = hashlib.sha256(lock_bytes).hexdigest()
    for record in [install, image]:
        if record.get('lockFile') != 'live-install-lock.json' or record.get('lockSha256') != actual:
            raise ValueError('adapter lock differs from retained installation/image receipt')
    if len(native) != 2 or any(row.get('packageLockSha256') != actual for row in native):
        raise ValueError('adapter lock differs from native login proof receipts')
    packages = json.loads(lock_bytes)['packages']
    for name, provider in install['providers'].items():
        resolved = packages['node_modules/' + name]
        if any(resolved.get(key) != provider[key] for key in ['version', 'integrity']):
            raise ValueError('installed provider identity differs from lock: ' + name)


class AdapterReceipts(unittest.TestCase):
    def setUp(self):
        self.lock = (ROOT / 'live-install-lock.json').read_bytes()
        self.install = json.loads((ROOT / 'live-install.json').read_bytes())
        self.image = json.loads((ROOT / 'container-arm64-image.json').read_bytes())
        self.native = [json.loads((ROOT / name).read_text().splitlines()[0]) for name in
                       ['claude-agent-acp-native-login.jsonl', 'codex-acp-native-login.jsonl']]

    def test_retained_receipts_match(self):
        verify(self.lock, self.install, self.image, self.native)

    def test_changed_lock_requires_new_evidence(self):
        with self.assertRaisesRegex(ValueError, 'lock differs'):
            verify(self.lock + b'\n', self.install, self.image, self.native)

    def test_provider_version_cannot_drift_behind_matching_hash(self):
        self.install['providers']['@agentclientprotocol/codex-acp']['version'] = '0.0.0'
        with self.assertRaisesRegex(ValueError, 'provider identity'):
            verify(self.lock, self.install, self.image, self.native)

    def test_optimized_python_cannot_silently_skip_probe_assertions(self):
        import subprocess
        import sys
        for name in ['check-acp-probe.py', 'check-acp-tool-probe.py']:
            result = subprocess.run([sys.executable, '-O', str(ROOT.parents[2] / 'scripts' / name)],
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('require assertions', result.stderr)


if __name__ == '__main__':
    unittest.main()
