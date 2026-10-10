"""Fixed offline Rust/Node workload; mounted read-only by the test harness.

This exercises the declared toolchains; it is not an agent-authored feature or
independent code review. No dependency installation or provider access occurs.
"""
import json
import os
from pathlib import Path
import subprocess

root = Path.cwd() / "qualification-output"
root.mkdir()  # A retry or pre-created output is not a clean measurement.
env = {key: os.environ[key] for key in ("HOME", "TMPDIR", "LANG") if key in os.environ}
env.update(PATH="/usr/local/bin:/usr/bin:/bin", CARGO_NET_OFFLINE="true",
           CARGO_HOME=str(root / "cargo-home"))

def run(args):
    result = subprocess.run(args, cwd=root, env=env, stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=120)
    if result.returncode != 0:
        # Only fixed offline commands with a credential-free environment run here.
        raise RuntimeError("fixed workload command failed: " + " ".join(args)
                           + "\n" + result.stderr[-4096:])
    return result.stdout

(root / "src").mkdir()
(root / "Cargo.toml").write_text('[package]\nname="qualification"\nversion="0.1.0"\nedition="2021"\n')
(root / "src/lib.rs").write_text('''pub fn sum(values: &[i64]) -> i64 { values.iter().sum() }
#[cfg(test)] mod tests {
    #[test] fn empty() { assert_eq!(super::sum(&[]), 0); }
    #[test] fn signed() { assert_eq!(super::sum(&[3, -5, 8]), 6); }
}
''')
(root / "package.json").write_text('{"private":true,"type":"module","scripts":{"test":"node --test test.mjs"}}\n')
(root / "sum.mjs").write_text('export const sum = values => values.reduce((a, b) => a + b, 0);\n')
(root / "test.mjs").write_text('''import test from 'node:test';
import assert from 'node:assert/strict';
import {sum} from './sum.mjs';
test('empty', () => assert.equal(sum([]), 0));
test('signed', () => assert.equal(sum([3, -5, 8]), 6));
''')
versions = {tool: run([tool, "--version"]).strip() for tool in ("rustc", "cargo", "node", "npm")}
run(["cargo", "build", "--offline"])
rust = run(["cargo", "test", "--offline"])
assert "test result: ok. 2 passed; 0 failed" in rust
run(["node", "--check", "sum.mjs"])
node = run(["node", "--test", "test.mjs"])
assert "# pass 2" in node and "# fail 0" in node
(root / "receipt.json").write_text(json.dumps({"schema": 1, "passed": True,
    "rustTests": 2, "nodeTests": 2, "versions": versions, "dependencyDownloads": 0}) + "\n")
print("kranz-resource-workload-passed", flush=True)
