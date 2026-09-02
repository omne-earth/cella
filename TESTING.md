# Testing cella

Two tiers, split by whether they need `/dev/kvm`:

- **No KVM needed** (`make test`): `cargo test --lib`, `cargo test --tests`,
  `make test-jail`, `make test-seccomp`. These run in this repo's own dev sandbox,
  in CI, in a container -- anywhere. They're also where almost all the
  *real* verification lives, because they exercise actual compiled code
  (the virtio-mmio register protocol, the block device's descriptor-chain
  walking, the freeze/thaw sidecar format, the installed BPF filter)
  rather than describing it.
- **Needs real KVM + a kernel/rootfs** (`make smoke-boot` / `make smoke-thaw` /
  `make smoke-ping`): one target per feature that only exists once a guest can
  actually run. These are the ones that touch the boot path (GDT, page
  tables, bzImage loading) that nothing else in this repo tests -- see
  the README's "What to check first."

`make test-all` runs everything. The KVM-dependent targets `SKIP` (exit
0) cleanly when `/dev/kvm`, the test assets, or the TAP device aren't
present, so `make test-all` still passes in an environment without KVM;
it just tells you what it couldn't check.

## Quick repro

```sh
git clone <this repo> && cd cella
make test          # ~2s, no KVM needed, this is the one to run first
```

Expected output ends with:
```
=== make test: all no-KVM checks passed ===
```

To also exercise the boot path, on a real Linux machine with KVM:

```sh
make init          # once per host: deps, toolbox, tap0, goldens
make golden        # the canonical goldens (no-op if init already did it)
make smoke-boot    # boots a real kernel all the way to init
make smoke-thaw    # boot -> freeze -> thaw -> verify
make smoke-ping    # the valve end to end: closed, open, release, closed again
```

Or all at once: `make smoke` -- it runs the no-KVM `make test` first,
then every gate (or `make test-all` for everything).

## What each target actually checks

| Target | Needs KVM? | What it verifies | Backing script |
|---|---|---|---|
| `make unit-test` | no | GDT/page-table bit-packing; freeze/thaw sidecar round-trip, corruption handling, one-shot enforcement; the BPF program's logic (every allowed syscall resolves to ALLOW, everything else to KILL) | inline `#[cfg(test)]` in `src/boot/x86_64.rs`, `src/freeze.rs`, `src/seccomp.rs` |
| `make integration-test` | no | virtio-blk read/write/read-only-enforcement/capacity through **real descriptor chains** against a **real backing file**; the virtio-mmio v2 register protocol (magic/version/feature-negotiation/status-reset/queue-notify/config-space) against the **real `MmioTransport`**, with a mock `IrqLine` standing in for the one KVM ioctl this layer needs | `tests/virtio_block.rs`, `tests/virtio_mmio.rs` |
| `make test-jail` | no | bwrap actually denies a path outside the bind set; `jail.sh` refuses to run without required args; no ambient `CAP_NET_ADMIN` inside the jail | `scripts/test/jail.sh` |
| `make test-seccomp` | no | the **actual installed BPF filter** kills the process with `SIGSYS` on a disallowed syscall (not a simulation -- `install()` really runs, then a real forbidden syscall is really made) | `scripts/test/seccomp.sh`, hitting `cella --selftest-seccomp` |
| `make smoke-boot` | **yes** | a real bzImage boots under real KVM all the way to a running init -- GDT/page-table/boot_params, virtio-mmio device negotiation, and a successful root mount, not just an early kernel banner | `scripts/test/boot.sh` |
| `make smoke-thaw` | **yes** | full lifecycle through the verbs: create, start, freeze (sidecar present, no leftover `.tmp`), thaw, `state` gone afterward (one-shot enforcement); the clock probe follows | `scripts/test/thaw.sh` |
| `make smoke-shell` | **yes** | the end-to-end story: a shell learns a value through `enter`, the machine freezes to files, thaws, and the same shell returns the value -- the one gate that drives the console interactively | `scripts/test/shell.sh` |
| `make smoke-ping` | **yes** | the valve end to end: a machine born closed answers nothing; open parks the guest's echo reply and the park is the freeze; a release answers the next ping; close darkens even the allowed flow | `scripts/test/ping.sh` |
| `make smoke-device-state` | **yes** | the four device-state acceptance gates: the disk survives the thaw (rw root), the network survives the thaw, a parked egress request completes after the thaw, and the world-ratchet -- the verdict external, against real endpoints | `scripts/test/device-state.sh` |
| `make smoke-multinet` | **yes** | a machine takes N taps: both nics present in the guest, the parked echo reply is decided and eth0 answers, a claimed tap of the list is refused again, and a freeze and thaw carry two net transports in the sidecar -- the new epoch decides its reply again | `scripts/test/multinet.sh` |
| `make smoke-ledger` | **yes** | the chronicle end to end: one fetch parks as one operation with an id and both clocks, the operation survives a freeze and thaw as held, a release by id completes it with no phantom, and two operations release strictly in park order | `scripts/test/ledger.sh` |
| `make smoke-gateway-cli` | **yes** | the gateway verbs: born closed asserted, open arms the membrane, show lists the hold, release by id prefix through the thaw, refuse lapses with its why, close darkens even the once-allowed, and a fresh epoch parks it again | `scripts/test/gateway-cli.sh` |
| `make smoke-gateway` | **yes** | the appliance between an agent and the world: pair wiring (an addressless bridge and the host route), the agent reaches the host only through the appliance, and the pair freezes together (agent first) and thaws together (appliance first) with the world still reachable | `scripts/test/gateway.sh` |
| `make smoke-universe` | **yes** | the universe family end to end: a frozen branch yields twins that thaw to the same instant, a rock refuses start/thaw/enter, the inspect appliance mounts the evidence at /rock (must read back, a write must fail loudly), the inspector dies on detach, the evidence stays byte-identical, and a branched rock stays a rock | `scripts/test/universe.sh` |
| `make doctor` | no | the host facts (one line each, nonzero on FAIL) and every golden digest against its manifest | `cella doctor check` + `cella doctor verify` |
| `make probe-wallclock` / `probe-freeze-thaw-clock` / `probe-sregs` | **yes** (sregs: no guest) | the cryogenic clock gates, through the installable `cella-probe` binary -- no cargo at run time | `cella probe <name>` |

## Why the KVM-dependent tests are separate, not skipped-by-default unit tests

`cargo test` has no built-in way to skip a test cleanly when a whole
class of hardware access is unavailable, and a `#[test]` that panics
with "no /dev/kvm" on every CI run is worse than useless -- it trains
people to ignore red. Shelling out from `make` lets each script decide
SKIP vs. FAIL for itself (missing `/dev/kvm` is a skip; a process that
exited before printing anything is a fail) and print exactly why.

## Adding a new feature test

Follow the `make smoke-thaw` / `scripts/test/thaw.sh` pattern:

1. Write `scripts/<feature>.sh`: check preconditions and `SKIP` (exit 0)
   if unmet, build what it needs via `$BIN`/`$SCRIPTS` variables (so it
   works standalone or via `make`), do the real thing against the
   compiled binary, assert on an observable outcome, clean up with a
   `trap ... EXIT`.
2. Add a `<feature>: build ## ...` target to the `Makefile` that just
   calls the script.
3. Add it to `smoke`'s prerequisite list if it's KVM-dependent, or to
   `test`'s if it isn't.
4. Add a row to the table above.

## Line counts

`make lines` (backed by `scripts/utils/count_lines.py`, which separates real
source from inline `#[cfg(test)]` blocks rather than just running `wc
-l` over everything):

```
src/ (excluding inline #[cfg(test)] blocks)               2063
src/ inline #[cfg(test)] test modules                      399
tests/ (integration tests)                                 420
--------------------------------------------------------------
SOURCE ONLY                                               2063
SOURCE + ALL TESTS (inline + tests/)                      2882
```
