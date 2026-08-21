# M7 container per-host egress live proof

Date: 2026-08-21

Scope: `provider: "container"`, `enforce: "fs+net"`, non-empty per-host
egress on Docker. This receipt does not widen the Windows provider claim or
claim equivalent Podman/nerdctl/Apple-container networking.

## Boundary under test

- Worker: unique Docker internal network, read-only root, no default route.
- Relay: unique dual-homed container, internal alias `kranz-egress`, running
  as the private files' exact numeric owner with all Linux capabilities
  dropped, no-new-privileges, and a read-only root.
- Filter: one host-side Kranz CONNECT proxy with the existing effective
  allowlist and structured per-run denial sink.
- Attribution: 256-bit per-run bearer token copied through Docker's local copy
  API into a private volume attached to a stopped, networkless loader
  container. No image code runs and no host credential path is bind-mounted.
  The loader and 0700/0600 host copies are deleted before worker spawn; only
  the relay later mounts the volume, read-only. The relay injects the token;
  unauthenticated CONNECT gets 407 and creates no denial.
- Supply chain: relay manifest pinned to
  `python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d`.

Docker's internal-network contract supplies external isolation/no default
gateway, and its network-connect primitive supplies the relay's deliberate
second attachment: [Docker networking](https://docs.docker.com/engine/network/),
[docker network connect](https://docs.docker.com/reference/cli/docker/network/connect/).

## Local hostile receipt

Host: macOS operator host, Colima Docker engine 29.2.1 on Ubuntu 24.04.4 LTS
arm64. Exact fixture:

```text
cargo test -p kranz-engine --lib container_egress::tests::container_per_host_egress_live_proof -- --exact --ignored --nocapture
test container_egress::tests::container_per_host_egress_live_proof ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured
```

The fixture proved, against the live daemon:

1. CONNECT to a loopback echo target explicitly present in the allowlist
   returned `200 Connection Established` and tunneled `ping`/`pong`.
2. CONNECT to `denied.invalid:443` returned 403 and shutdown returned exactly
   one `EgressDenial { host: "denied.invalid", port: 443 }`.
3. An ordinary bridge control container reached a live external peer. A
   worker on the internal network, with no proxy involvement, could not open
   the same direct socket. This is the anti-vacuity control for the bypass.
4. Host credential staging and the stopped loader were absent before the
   first worker spawn. Normal shutdown removed the worker name, relay,
   network, private credential volume, and staging directory.
5. Error/timeout-shaped Drop force-removed a still-running daemon-owned worker
   before removing the relay, network, credential volume, and staging
   directory.
6. A seeded stale owner (dead PID/token) plus live stale relay, stopped loader,
   network, credential volume, and staging directory were removed by recovery;
   a live owner is retained by the same identity comparison.

Post-proof `docker ps -a --filter name=kranz-egress` and
`docker network ls --filter label=com.kranz.egress-boundary=true` both
returned empty output.

## Linux CI receipt

The dedicated `rust-linux-container-egress` job runs the same exact ignored
fixture on `ubuntu-latest` and enforces `test result: ok. 1 passed` as an
anti-vacuity check. Its first trusted run URL and SHA are intentionally filled
only after the proposed implementation executes green on Linux; until then
the ticket remains open.
