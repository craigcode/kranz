#!/usr/bin/env bash
# Structural guard for .github/workflows/ci.yml: verifies the macOS test job
# exists and is wired correctly, and that all `uses:` refs stay SHA-pinned.
set -uo pipefail

CI_YML="${1:-.github/workflows/ci.yml}"

if [ ! -f "$CI_YML" ]; then
  echo "FAIL: $CI_YML does not exist" >&2
  exit 1
fi

ruby -ryaml -e '
ci_path = ARGV[0]

# (1) file parses as YAML
begin
  doc = YAML.safe_load(File.read(ci_path), aliases: true)
rescue => e
  warn "FAIL: #{ci_path} does not parse as YAML: #{e.message}"
  exit 1
end

jobs = doc["jobs"] || {}

# (2) some job has runs-on: macos-latest
macos_job_name, macos_job = jobs.find { |_, j| j.is_a?(Hash) && j["runs-on"] == "macos-latest" }

if macos_job.nil?
  warn "FAIL: no job with runs-on: macos-latest found"
  exit 1
end

steps = macos_job["steps"] || []
run_lines = steps.map { |s| s["run"] }.compact.join("\n")

# (3) macOS job runs cargo test --workspace with --no-fail-fast
has_test = run_lines.each_line.any? { |l| l.include?("cargo test") && l.include?("--workspace") && l.include?("--no-fail-fast") }
unless has_test
  warn "FAIL: macOS job #{macos_job_name.inspect} does not run \"cargo test --workspace ... --no-fail-fast\""
  exit 1
end

# (4) macOS job does NOT invoke cargo fmt or cargo clippy
if run_lines.each_line.any? { |l| l.include?("cargo fmt") }
  warn "FAIL: macOS job #{macos_job_name.inspect} must not run cargo fmt"
  exit 1
end
if run_lines.each_line.any? { |l| l.include?("cargo clippy") }
  warn "FAIL: macOS job #{macos_job_name.inspect} must not run cargo clippy"
  exit 1
end

# (5) macOS job uses Swatinem/rust-cache
uses_values = steps.map { |s| s["uses"] }.compact
unless uses_values.any? { |u| u.start_with?("Swatinem/rust-cache") }
  warn "FAIL: macOS job #{macos_job_name.inspect} does not use Swatinem/rust-cache"
  exit 1
end

# (6) every uses: value anywhere in the file is pinned to a 40-hex commit SHA
all_uses = []
collect = lambda do |node|
  case node
  when Hash
    node.each do |k, v|
      all_uses << v if k == "uses" && v.is_a?(String)
      collect.call(v)
    end
  when Array
    node.each { |v| collect.call(v) }
  end
end
collect.call(doc)

sha_re = /@[0-9a-f]{40}(\s|$)/
unpinned = all_uses.reject { |u| u =~ /@[0-9a-f]{40}/ }
unless unpinned.empty?
  warn "FAIL: the following uses: values are not pinned to a 40-hex commit SHA: #{unpinned.inspect}"
  exit 1
end

warn "PASS: all checks succeeded (macOS job: #{macos_job_name.inspect})"
exit 0
' "$CI_YML"
