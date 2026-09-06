# Kranz container image (M6 cloud missions — PREVIEW, not yet exercised e2e).
#
# Builds the `kranz` binary and ships it on a minimal Debian runtime with git.
# The image is deliberately scoped to "kranz + git": it is enough to run
# `kranz serve` or a headless `kranz exec` once the two deployment-specific
# pieces below are layered on. See docs/deploy.md for the full runbook.
#
# What this image intentionally does NOT bundle (they are deployment-specific):
#   1. The `claude` CLI (needs Node.js). Kranz shells out to it for every
#      agent turn; at runtime it must be on PATH or pointed at by
#      KRANZ_CLAUDE_BIN, and ANTHROPIC_API_KEY must be set (cloud auth path,
#      not local OAuth). See the runtime TODO below.
#   2. Linux bubblewrap and the mission's own target toolchain (cargo, node,
#      go, python, …). A real mission image layers the containment primitive
#      and repo toolchain on top — via the repo's devcontainer.json when
#      present, or a fatter base otherwise (roadmap M6, "Workspace
#      provisioning"). Keeping those out here keeps the base small.

# ---- builder -----------------------------------------------------------------
# rust:1 tracks the latest 1.x; the workspace needs Rust 1.88+ (edition 2021,
# README quick-start). The committed dashboard dist under
# crates/cli/assets/dashboard/dist is embedded by crates/cli/build.rs, so the
# release build needs no Node.js — pure cargo.
FROM rust:1-slim@sha256:17d1ba895198f9934c6314ec5346a0d5115372f3243390c3d731e242f35c2f27 AS builder

# git is present in the builder for any build script that inspects the repo;
# the release build itself does not require it.
RUN apt-get update \
    && apt-get install -y --no-install-recommends git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# Build only the CLI crate (package `kranz`, binary name: `kranz`) in release.
RUN cargo build --release --locked -p kranz \
    && strip target/release/kranz \
    && cp "$(rustc --print sysroot)/share/doc/rust/COPYRIGHT-library.html" /tmp/RUST_LIBRARY_NOTICES.html

# ---- runtime -----------------------------------------------------------------
FROM debian:stable-slim@sha256:1710bde34461551a19a47c787885ec9ad7058d9a5bead2affb8d088fa2f8502b AS runtime

# git: Kranz shells out to the git binary for every mission ref (git_ops.rs).
# ca-certificates: TLS trust for the scoped push to a remote and for the
# claude CLI's HTTPS calls to the Anthropic API.
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Non-root user. /work is the mission working tree (a clone of the target repo);
# .kranz mission data lives under it. Owned by the unprivileged user.
RUN useradd --create-home --uid 10001 kranz
WORKDIR /work
RUN chown kranz:kranz /work

# The executable carries project/dependency notices via `kranz licenses`;
# retain standalone copies and the compiler's library inventory in the image.
COPY --from=builder /src/target/release/kranz /usr/local/bin/kranz
COPY --from=builder /src/LICENSE /usr/share/doc/kranz/LICENSE
COPY --from=builder /src/crates/cli/assets/THIRD_PARTY_NOTICES.txt /usr/share/doc/kranz/THIRD_PARTY_NOTICES.txt
COPY --from=builder /src/crates/cli/assets/dashboard/dist/THIRD_PARTY_NOTICES.txt /usr/share/doc/kranz/DASHBOARD_THIRD_PARTY_NOTICES.txt
COPY --from=builder /tmp/RUST_LIBRARY_NOTICES.html /usr/share/doc/kranz/RUST_LIBRARY_NOTICES.html

# TODO (deployment-specific, see docs/deploy.md):
#   - Provide the `claude` CLI on PATH. The usual path is a small Node layer:
#       RUN apt-get update && apt-get install -y --no-install-recommends nodejs npm \
#         && npm install -g @anthropic-ai/claude-code && rm -rf /var/lib/apt/lists/*
#     or COPY a pre-installed claude and set KRANZ_CLAUDE_BIN to its path.
#   - Layer the mission's target toolchain (devcontainer.json or a fat image).
#   - Install bubblewrap before enabling Linux process-sandbox enforcement.
#
# Required at runtime (NOT baked into the image — pass at `docker run`):
#   - ANTHROPIC_API_KEY  (cloud auth for the claude CLI)
#   - a deploy key scoped to kranz/* refs, if the mission pushes its branch
#   - KRANZ_* config as needed (see docs/deploy.md)

USER kranz

# `kranz` is the entrypoint; supply the subcommand + args at run time, e.g.
#   docker run ... kranz-image exec -f mission.md          (ephemeral)
#   docker run ... kranz-image serve --port 4560 --slack   (persistent host)
ENTRYPOINT ["kranz"]
