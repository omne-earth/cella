# Testing cella

Everything runs through make targets. Never run a gate script or a
cargo command directly: the targets set up logging, prerequisites,
and the environment, and a result that did not come from a target
is not a result. If a target is missing, add one.

## The two tiers

Split by whether the check needs `/dev/kvm`:

- **`make test` -- no KVM.** Build hygiene (check, lint, fmt), the
  unit and integration tests, the jail gate, the per-binary seccomp
  gates, the machine-registry gate, the one-door static gate, and
  the witness-doors gate. Runs anywhere: CI, a container, a
  sandbox. Takes seconds. Run it first, always.
- **`make smoke` -- real KVM, a real guest.** One gate per
  workflow, each booting real kernels under real KVM. Takes tens
  of minutes. Every gate SKIPs cleanly (exit 0, with a reason)
  when `/dev/kvm` or the goldens are absent.

`make test-all` runs both tiers.

## Quick start

From nothing to a green battery:

```sh
git clone <this repo> && cd cella
make test          # seconds, no KVM -- run this first
make init          # once per host: deps, toolbox, sub-id delegation, goldens (the one sudo moment)
make smoke         # the full battery
```

`make test` must end with:

```
=== make test: all no-KVM checks passed ===
```

`make smoke` must end with:

```
=== make smoke: done (see above for any SKIPs) ===
```

A SKIP is loud and named. If you did not read a SKIP reason, you
do not know what was not checked.

## The battery, one part per CLI

`make smoke` chains six parts, one per binary. A red part names an
accused binary -- the same granularity as the per-binary jail,
seccomp list, and SELinux domain. Each part runs standalone.

| Part | Accused binary | Gates it runs |
|---|---|---|
| `make smoke-cella-doctor` | cella-doctor | rootless sweep, doctor check + verify |
| `make smoke-cella-vmm` | cella-vmm | shell, boot, device-state (AC1-AC5) |
| `make smoke-cella-machine` | cella-machine | thaw, machine (selftest), clean, nested-boot (3 variants) |
| `make smoke-cella-gateway` | cella-gateway | ping, udp, collide, gateway, gateway-cli, inspection, ledger, chain |
| `make smoke-cella-network` | cella-network | wire, world, multinet, translator-port-neg |
| `make smoke-cella-probe` | cella-probe | witness, universe, probe-inception |

The order is blame direction: ground first (doctor), then the VMM,
then the verbs, then the border, then the wires, then the
instruments. A broken earlier part makes later failures noise.

## What each gate proves

The one-line law of every target lives in the Makefile, above the
target, and `make help` renders it. The map from gate to law:

| Gate | Proves | Law lives in |
|---|---|---|
| `test-jail` | bwrap actually confines: a path outside the bind set is denied | scripts/test/jail.sh |
| `test-seccomp` + `test-seccomp-<persona>` | the installed BPF filter kills on a forbidden syscall, per binary, for real | scripts/test/seccomp.sh |
| `test-machine` | the registry verbs against a sandboxed CELLA_HOME | scripts/test/machine.sh |
| `test-one-door` | exactly one TX call site writes the edge -- the decision-delivery door, statically | inline in the Makefile |
| `test-witness` | six witness doors, one per persona; the shim owns none | inline in the Makefile |
| `smoke-boot` | a real bzImage to a running init; the release flavor boots dark | scripts/test/boot.sh |
| `smoke-shell` | a shell learns a value, freezes to files, thaws, remembers | scripts/test/shell.sh |
| `smoke-thaw` | create, start, freeze, thaw, the one-shot sidecar, then the clock probe | scripts/test/thaw.sh |
| `smoke-ping` | the valve end to end, both lanes: closed dark, the knock parks without freezing, release is live, the reply's park is the freeze, reopened remembers nothing | scripts/test/ping.sh |
| `smoke-udp` | no datagram crosses undecided, proven from inside the guest and from the world's side | scripts/test/udp.sh |
| `smoke-collide` | the matcher never guesses: ambiguity holds everything, delivers nothing | scripts/test/collide.sh |
| `smoke-inspection` | sight requires stillness: inspect renders frozen holds, seals the sealed, witnesses the look | scripts/test/inspection.sh |
| `smoke-ledger` | the chronicle: parks with ids and both clocks, releases strictly in park order | scripts/test/ledger.sh |
| `smoke-chain` | field 15 chains both books; a tampered book snaps loudly | scripts/test/chain.sh |
| `smoke-gateway-cli` | the gateway verbs, one by one, against the record | scripts/test/gateway-cli.sh |
| `smoke-gateway` | the appliance shape: an agent reaches the world only through its gateway machine, and the pair freezes and thaws together | scripts/test/gateway.sh |
| `smoke-wire` | two machines, one wire, no host object; the frozen peer's mail discards and counts | scripts/test/wire.sh |
| `smoke-world` | sockets instead of taps: ARP and the gateway echo answered at the edge, ICMP/UDP/TCP crossing decided both ways, the knock parks | scripts/test/world.sh |
| `smoke-multinet` | N nics on one machine, every crossing decided per nic | scripts/test/multinet.sh |
| `smoke-translator-port-neg` | the tether: an rm without destroy orphans no translator, and the knock port frees | scripts/test/translator-port-neg.sh |
| `smoke-rootless` | no capability on any binary, no tap, bridge, nft table, or boot unit of cella's on the host | scripts/test/rootless.sh |
| `smoke-device-state` | AC1-AC5: disk, network, exact in-flight state, external verdict, the true world | scripts/test/device-state.sh |
| `smoke-nested-boot` | cella hosts cella, three network variants, at real nesting depth | scripts/test/nested-boot.sh |
| `smoke-machine` | the lifecycle cycle end to end: cella selftest | via cella selftest |
| `smoke-universe` | branch, archive, inspect: machines as artifacts, rocks stay rocks | scripts/test/universe.sh |
| `smoke-witness` | every verb is an event, in the right book, with uid, gid, persona | scripts/test/witness.sh |
| `probe-inception` | the cryogenic clock, one nesting level down | via cella probe |
| `smoke-engine` (engine-w1..w5) | the world-engine seam: the stream stands, decisions land, stillness on engine halt, the frozen machine, two judges (docs/WORLD-ENGINE.md, "The gates") | scripts/test/engine.sh |

Design detail lives with the law: docs/NETWORK-MODEL.md (the
membrane), docs/ROOTLESS-NETWORK.md (the translator),
docs/FREEZE-THAW.md (time), docs/DEVICE-STATE.md (AC1-AC5),
docs/NESTED-BOOT.md (the recursion), docs/EXAMPLES.md (the
shapes, E1-E6).

## Logs

Every target tees its output to `.logs/<target>-<timestamp>.log`.
When a battery fails:

1. Do not rerun blindly. Read the newest log for the failing
   target: the FAIL line names the step and the assertion.
2. Rerun only the failing part (`make smoke-cella-network`), or
   the single gate (`make smoke-wire`).
3. Never edit source while a battery runs: the results after the
   edit are tainted, and the battery restarts from clean.

On a shared checkout (a bare-metal run), the same `.logs/`
directory carries the results back; read them from there.

## Knobs

| Knob | Effect |
|---|---|
| `CELLA_KEEP_SANDBOX=1` | a gate keeps its temporary CELLA_HOME on exit and prints the path, instead of destroying it -- for post-mortem only |
| `CELLA_THAW_PREFAULT=off\|ept\|deep` | the thaw prefill mode; `deep` is the default (docs/FREEZE-THAW.md) |
| `WORLD_PORT` | chosen at random (1024-9999) by each gate, per run -- a leaked bind can never poison a later gate; do not pin it |

## SKIP vs FAIL

A script decides for itself, and says why:

- **SKIP (exit 0)**: a precondition is absent -- no `/dev/kvm`, no
  goldens, no built binary. The battery stays green and the reason
  prints. `cargo test` cannot express this; the scripts can, which
  is why the KVM tier is scripts behind make and not `#[test]`s
  that panic red on every KVM-less CI run.
- **FAIL (exit 1)**: a precondition held and an assertion broke.
  A process that died before printing is a FAIL, never a SKIP.

## Adding a gate

Follow scripts/test/wire.sh as the template:

1. Write `scripts/test/<feature>.sh`: check preconditions and SKIP
   (exit 0) if unmet; work inside a temporary CELLA_HOME
   (`mktemp -d`), honoring `CELLA_KEEP_SANDBOX`; pick
   `WORLD_PORT=$(( (RANDOM % 8976) + 1024 ))` if it knocks; assert
   observable outcomes; tear down with `trap ... EXIT`, and
   `destroy` every machine before any `rm` -- an rm alone would
   orphan nothing since the tether, but destroy is the contract.
2. Add the target to the Makefile: the `## law` line above it, the
   `$(LOG)` first recipe line, the script call.
3. Roster it: `SMOKE_TARGETS` (the log/help roster), the `.PHONY`
   line, and the per-CLI part whose binary it accuses.
4. Add its row to the table above.

## Line counts

`make lines` separates real source from test code
(scripts/utils/count_lines.py). As of 2026-09-03:

```
SOURCE ONLY (all crates)                  14281
SOURCE + ALL TESTS (inline + tests/)      16018
```

Ten crates; the largest is cella-vmm, the smallest is the shim
(cella, ~100 lines: a routing table and one exec).
