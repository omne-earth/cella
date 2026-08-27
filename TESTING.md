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
  `make smoke-net`): one target per feature that only exists once a guest can
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
make init          # once per host: deps, toolbox, tap0, dist
make dist          # rootfs + kernel build (no-op if init already did it)
make smoke-boot    # boots a real kernel, watches serial
make smoke-thaw    # boot -> freeze -> thaw -> verify
make smoke-net     # best-effort ping over the TAP
```

Or all at once: `make smoke` (or `make test-all` for everything).

## What each target actually checks

| Target | Needs KVM? | What it verifies | Backing script |
|---|---|---|---|
| `make unit-test` | no | GDT/page-table bit-packing; freeze/thaw sidecar round-trip, corruption handling, one-shot enforcement; the BPF program's logic (every allowed syscall resolves to ALLOW, everything else to KILL) | inline `#[cfg(test)]` in `src/boot/x86_64.rs`, `src/freeze.rs`, `src/seccomp.rs` |
| `make integration-test` | no | virtio-blk read/write/read-only-enforcement/capacity through **real descriptor chains** against a **real backing file**; the virtio-mmio v2 register protocol (magic/version/feature-negotiation/status-reset/queue-notify/config-space) against the **real `MmioTransport`**, with a mock `IrqLine` standing in for the one KVM ioctl this layer needs | `tests/virtio_block.rs`, `tests/virtio_mmio.rs` |
| `make test-jail` | no | bwrap actually denies a path outside the bind set; `jail.sh` refuses to run without required args; no ambient `CAP_NET_ADMIN` inside the jail | `scripts/test/jail.sh` |
| `make test-seccomp` | no | the **actual installed BPF filter** kills the process with `SIGSYS` on a disallowed syscall (not a simulation -- `install()` really runs, then a real forbidden syscall is really made) | `scripts/test/seccomp.sh`, hitting `cella --selftest-seccomp` |
| `make smoke-boot` | **yes** | a real bzImage boots under real KVM far enough to print a kernel banner on the serial console -- the GDT/page-table/boot_params code path | `scripts/test/boot.sh` |
| `make smoke-thaw` | **yes** | full lifecycle: boot, `SIGUSR1` freeze, sidecar file exists with no leftover `.tmp`, re-invoking the same command line thaws instead of re-booting, `state` is gone afterward (one-shot enforcement) | `scripts/test/thaw.sh` |
| `make smoke-net` | **yes** | guest answers ICMP over the TAP after boot -- best-effort, depends on the test rootfs configuring networking from the `ip=` kernel parameter, which is unverified (see the script's own caveat) | `scripts/test/net.sh` |

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
