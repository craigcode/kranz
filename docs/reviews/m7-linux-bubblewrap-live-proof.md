# M7 Linux bubblewrap fs+net live proof

Date: 2026-08-20

Host: Ubuntu 24.04.4, x86_64, Linux 6.17.0-1022-azure

Bubblewrap: 0.9.0

Node: 22.23.2

Rust: 1.98.0

## Result

The protected Ubuntu CI host ran the real Linux process-provider boundary with
`sandbox.enforce = "fs+net"`. A hostile command could not create a file in a
sibling temporary directory, and a contained Python process could not connect
to a live listener on the host loopback interface. The same connection passed
without enforcement, proving that the network denial was not a dead target or
broken probe.

An ordinary warm Node contract command passed inside bubblewrap. Seven
interleaved wrapped and unwrapped samples measured 1.11% median overhead, below
the roadmap's approximate 10% target. The primary checkout retained the same
HEAD and clean tracked bytes before and after the proof.

## Provenance

- Pull request: `#10`, `codex/m7-linux-live-proof`
- Proof branch SHA: `16ab67a2c1c0e5b7162f922226865d5014c9e0d4`
- GitHub test-merge SHA: `b111ab831ccb5740ab5ccd2a586fbaa2f9277ef2`
- Workflow run: `32411486438`
- Ubuntu job: `96562696734`
- Exact test:
  `command_exec::tests::linux_bubblewrap_hostile_live_receipt`
- Command:
  `cargo test -p kranz-engine --lib command_exec::tests::linux_bubblewrap_hostile_live_receipt -- --exact --ignored --nocapture`

The hosted runner installed Ubuntu's `bubblewrap 0.9.0-1ubuntu0.1`. Because the
runner image restricts unprivileged user namespaces through AppArmor, the
ephemeral proof job enabled `kernel.unprivileged_userns_clone` and disabled
`kernel.apparmor_restrict_unprivileged_userns`, then required this capability
probe to pass before running the receipt:

```sh
bwrap --ro-bind / / --dev /dev --proc /proc --unshare-net true
```

This changes only the disposable CI host's ability to create the user/network
namespace; the filesystem and network denials below are enforced by the real
bubblewrap invocation used by Kranz.

## Hostile probes

The filesystem probe ran inside the resolved gate sandbox:

```sh
printf escaped > '<sibling-tempdir>/kranz-linux-hostile-canary'
```

It exited non-zero and the canary remained absent. The network probe created a
real host listener on `127.0.0.1:<ephemeral-port>` and attempted:

```sh
python3 -c 'import socket; socket.create_connection(("127.0.0.1", <port>), 2).close()'
```

The unwrapped command connected successfully. The `fs+net` command failed, and
the listener accepted exactly one connection: the anti-vacuity control only.

## Normal gate and timings

The warm-cache command was identical in both postures:

```sh
node -e "let n=0; for(let i=0;i<100000;i++)n=(n+i)>>>0; if(n!==704982704)process.exit(2); setTimeout(()=>console.log('kranz-linux-node-ok'),750)"
```

Both warm-ups and all retained samples exited zero and emitted the expected
marker. Samples were interleaved in alternating order; medians are the fourth
sorted values.

```text
off ms:        784.766 785.642 786.271 783.260 784.289 784.548 785.498
bubblewrap ms: 789.079 794.345 798.683 789.412 793.531 793.480 789.393
median:        784.766 ms off; 793.480 ms bubblewrap
delta:         +8.714 ms, +1.11%
target:        <= 10%; passed
```

These are observations from this host, not a universal performance claim.
Sandbox resolution is performed once per gate posture and excluded from the
per-command retained samples, matching production validation/final-gate use.

## Checkout integrity and cleanup

The test required a clean tracked checkout before starting, captured `HEAD`,
and asserted the same SHA and byte-clean tracked status after every hostile and
timing command. The workflow repeated those checks around the exact test. Test
repositories, scratch roots, the sibling canary directory, and the listener
were process-owned fixtures; they were dropped after the test, and the hosted
runner itself was disposable.

This closes the Linux live-proof ticket. It does not prove Windows
AppContainer containment or the separate container-provider per-host egress
boundary.
