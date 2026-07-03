# Homebrew formula for kranz (skeleton).
#
# This is a from-source formula: Homebrew fetches the release source tarball
# and runs `cargo install`. The two placeholders below are filled at release
# time (see docs/releasing.md, "Update the Homebrew formula"):
#
#   OWNER   -> the GitHub org/user that owns the repo
#   VERSION -> the tag being released, without the leading "v" (e.g. 0.1.0)
#             The url uses the tag "v#{version}"; keep them in sync.
#   sha256  -> shasum -a 256 of the downloaded tarball:
#                curl -sL https://github.com/OWNER/kranz/archive/refs/tags/vVERSION.tar.gz | shasum -a 256
#
# Ship it via a tap (e.g. `brew tap OWNER/kranz && brew install kranz`) or a
# homebrew-core submission once the project is public and stable.
class Kranz < Formula
  desc "Local mission-control harness for Claude Code"
  homepage "https://github.com/OWNER/kranz"
  # PLACEHOLDER: bump VERSION to match the released tag (url resolves to v#{version}).
  url "https://github.com/OWNER/kranz/archive/refs/tags/v0.1.0.tar.gz"
  version "0.1.0"
  # PLACEHOLDER: replace with the tarball's real sha256 (see header comment).
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"

  depends_on "rust" => :build

  def install
    # Build just the CLI crate (the `kranz` binary) into the Homebrew prefix.
    system "cargo", "install", *std_cargo_args(path: "crates/cli")
  end

  test do
    assert_match "kranz", shell_output("#{bin}/kranz --help")
  end
end
