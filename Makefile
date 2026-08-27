# Named directly rather than via `/usr/bin/env bash`: under .ONESHELL make
# decides whether to strip per-line '@'/'-' prefixes by looking at SHELL's
# basename, and "env" is not on its list of known Bourne-compatible shells.
SHELL := /bin/bash
# One shell per recipe (not per line), and a strict one: -e so a failing
# step stops the recipe instead of the next line papering over it, -u so
# a typo'd variable is an error rather than an empty string, -o pipefail
# so a failure mid-pipeline isn't hidden by a successful `tee`/`sort` at
# the end. .ONESHELL is what makes $(LOG), below, possible at all.
.ONESHELL:
.SHELLFLAGS := -ueo pipefail -c

# Every recipe's first line is `$(LOG)`. It tees the whole recipe's
# output -- stdout and stderr, including anything the scripts and probes
# print -- into .logs/<target>-<timestamp>.log while still showing it on
# the terminal, so a failed run always leaves evidence behind without
# anyone having to remember to redirect. Only works under .ONESHELL: the
# `exec` redirect has to apply to the rest of the recipe, which requires
# the whole recipe to be a single shell. Leading `@` silences make's own
# echo of the recipe text (the header line below says what ran instead).
# Target names with a `/` in them (dist/bzImage) become `dist_bzImage`
# so the log path stays one level deep.
LOGDIR := .logs
LOG = @mkdir -p $(LOGDIR); exec > >(tee -a "$(LOGDIR)/$(subst /,_,$@)-$$(date +%Y%m%d-%H%M%S).log") 2>&1; echo "=== make $@ -- $$(date -Is) ==="

CARGO ?= cargo
SCRIPTS := scripts
DIST := dist

# Local overrides, not committed -- copy .env.example to .env to change these.
-include .env
CELLA_TAP ?= tap0
CELLA_TAP_CIDR ?= 192.168.200.1/24

# Pinned, not resolved at build time. assets.sh used to ask kernel.org
# for the current longterm release on every single run, which means the
# kernel could -- and did -- move out from under a measurement
# mid-investigation: an RTC boot-time comparison silently spanned
# 6.18.46 -> 6.18.47 because the rebuild happened to cross a point
# release. `make dist` is now reproducible. Override either in .env (see
# the -include above) or on the command line to bump or bisect; find the
# current longterm at https://www.kernel.org/releases.json.
KERNEL_VERSION ?= 6.18.47
BUSYBOX_VERSION ?= 1.37.0
export KERNEL_VERSION BUSYBOX_VERSION

.PHONY: help build debug check lint fmt fmt-check \
        unit-test integration-test selftest test test-all \
        init dist setup-tap \
        smoke smoke-boot smoke-thaw smoke-net smoke-clean test-jail test-seccomp \
        clean distclean distclean-kernel distclean-rootfs lines \
        probe-sregs probe-wallclock probe-freeze-thaw-clock \
        kernel-config-check

help: ## Show this help
	$(LOG)
	echo "cella -- build, lint, and test targets"
	echo ""
	echo "Build:"
	grep -hE '^(build|debug|check|lint|fmt|fmt-check):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Tests that need no /dev/kvm (unit + integration, run anywhere):"
	grep -hE '^(unit-test|integration-test|selftest|test|test-jail|test-seccomp):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Smoke tests: real KVM, a real guest (one target per workflow):"
	grep -hE '^(smoke|smoke-boot|smoke-thaw|smoke-net|smoke-clean):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Setup:"
	grep -hE '^(init|dist|setup-tap|kernel-config-check):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Everything:"
	grep -hE '^(test-all|clean|distclean|distclean-kernel|distclean-rootfs|lines):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Probes: one-off diagnostics, run by hand, not part of test/smoke:"
	grep -hE '^(probe-sregs|probe-wallclock|probe-freeze-thaw-clock):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true

# --- Build ------------------------------------------------------------

build: ## Release build (target/release/cella)
	$(LOG)
	$(CARGO) build --release

debug: ## Debug build (target/debug/cella), faster to compile
	$(LOG)
	$(CARGO) build

check: ## cargo check, no codegen
	$(LOG)
	$(CARGO) check --all-targets

lint: fmt-check ## cargo clippy (all targets) + fmt-check
	$(LOG)
	$(CARGO) clippy --all-targets

fmt: ## Apply cargo fmt
	$(LOG)
	$(CARGO) fmt

fmt-check: ## Verify formatting without changing files (CI-friendly)
	$(LOG)
	$(CARGO) fmt -- --check

# --- Tests that need no /dev/kvm ---------------------------------------
#
# These are the ones that run in an ordinary container/CI runner. Split
# per kind (unit vs. integration) so `make unit-test` stays fast during
# iteration; `make test` runs everything in this section.

unit-test: ## cargo test --lib (inline #[cfg(test)] modules)
	$(LOG)
	$(CARGO) test --lib

integration-test: ## cargo test --tests (tests/*.rs, real virtio-mmio/blk logic, no KVM)
	$(LOG)
	$(CARGO) test --tests

selftest: build ## Sanity-run the seccomp self-test binary directly (see also: make test-seccomp)
	$(LOG)
	# `|| status=$$?` rather than a bare call: the whole point of this
	# target is a binary that exits 159 (SIGSYS), which `set -e` would
	# otherwise treat as a recipe failure before we can check for it.
	status=0
	./target/release/cella --selftest-seccomp || status=$$?
	if [ $$status -eq 159 ]; then
		echo "OK: killed by SIGSYS as expected (exit $$status)"
	else
		echo "UNEXPECTED exit $$status"
		exit 1
	fi

test-jail: build ## Rootless bwrap jail actually confines the process (scripts/test/jail.sh)
	$(LOG)
	$(SCRIPTS)/test/jail.sh

test-seccomp: build ## The real BPF filter kills a disallowed syscall (scripts/test/seccomp.sh)
	$(LOG)
	$(SCRIPTS)/test/seccomp.sh

test: check lint unit-test integration-test test-jail test-seccomp ## Everything above: build hygiene + all no-KVM tests
	$(LOG)
	echo ""
	echo "=== make test: all no-KVM checks passed ==="

# --- Smoke tests: real KVM, a real guest, real workflows ---------------
#
# Named smoke-* rather than bare boot/thaw/net: these aren't the
# operations themselves (that's `scripts/jail.sh` for actually running
# cella, `kill -USR1` for actually freezing it -- see README), they're
# pass/fail checks that those workflows still work end to end. One
# target per significant workflow, each a thin `make` wrapper around a
# script that does the real orchestration -- see scripts/test/boot.sh,
# scripts/test/thaw.sh, scripts/test/net.sh. All three SKIP (exit 0)
# cleanly if /dev/kvm, dist, or the TAP device aren't present, so
# `make smoke` doesn't hard-fail without KVM.

smoke-boot: build dist ## Boot a real kernel under KVM all the way to a running init (scripts/test/boot.sh)
	$(LOG)
	$(SCRIPTS)/test/boot.sh

smoke-thaw: build dist ## Boot -> freeze (SIGUSR1) -> verify sidecar -> thaw -> one-shot check (scripts/test/thaw.sh)
	$(LOG)
	$(SCRIPTS)/test/thaw.sh

smoke-net: build dist ## Guest answers ICMP over the TAP after boot (scripts/test/net.sh, best-effort)
	$(LOG)
	$(SCRIPTS)/test/net.sh

smoke: smoke-boot smoke-thaw smoke-net ## All three smoke-* targets (skips gracefully without KVM)
	$(LOG)
	echo ""
	echo "=== make smoke: done (see above for any SKIPs) ==="

smoke-clean: ## Kill any stray cella process left running by an interrupted smoke test
	$(LOG)
	pkill -f 'target/(release|debug)/cella' && echo "cella: killed stray process(es)" || echo "cella: nothing to clean up"

# --- Setup --------------------------------------------------------------

.toolbox: ## Sentinel: creates + provisions the cella-build toolbox (kernel build toolchain lives there, not on the host)
	$(LOG)
	$(SCRIPTS)/build/toolbox.sh
	touch .toolbox

init: ## One-time host setup (Fedora): installs runtime deps, provisions the build toolbox, creates tap0, builds dist, checks /dev/kvm (needs sudo)
	$(LOG)
	$(SCRIPTS)/setup/bootstrap.sh
	$(MAKE) .toolbox
	$(MAKE) setup-tap
	$(MAKE) dist

$(DIST)/bzImage $(DIST)/rootfs.ext4: | .toolbox
	$(LOG)
	$(SCRIPTS)/build/assets.sh

dist: $(DIST)/bzImage $(DIST)/rootfs.ext4 ## Build a minimal rootfs + bzImage kernel from source (compiled inside the toolbox), skipped if already built

kernel-config-check: ## Resolve kernel-fragment.config against defconfig and report any line kconfig silently overruled (seconds, no compile)
	$(LOG)
	$(SCRIPTS)/build/kernel-config-check.sh

setup-tap: ## One-time (per boot) TAP device creation -- needs sudo once (name/CIDR from .env, see .env.example)
	$(LOG)
	sudo $(SCRIPTS)/setup/tap.sh $(CELLA_TAP) $(CELLA_TAP_CIDR)

# --- Everything -----------------------------------------------------

test-all: test dist smoke ## make test, plus every KVM smoke test (skips gracefully without KVM)
	$(LOG)
	echo ""
	echo "=== make test-all: done (see above for any SKIPs) ==="

lines: ## Report source-only and source+test line counts (see also README's line-count section)
	$(LOG)
	python3 $(SCRIPTS)/utils/count_lines.py

clean: ## cargo clean
	$(LOG)
	$(CARGO) clean

distclean: clean ## clean + remove built dist/ assets
	$(LOG)
	rm -rf $(DIST)

distclean-rootfs: ## Remove just dist/rootfs.ext4, so the next `make dist` rebuilds the rootfs (for a rootfs.sh change)
	$(LOG)
	# The busybox build in target/rootfs-build survives, so the rebuild
	# only assembles the image again.
	rm -f $(DIST)/rootfs.ext4
	echo "cella: removed $(DIST)/rootfs.ext4 -- next 'make dist' rebuilds the rootfs"

distclean-kernel: ## Remove just dist/bzImage, so the next `make dist` rebuilds the kernel (for a kernel-fragment.config change)
	$(LOG)
	# Deliberately narrower than distclean: the cached kernel source tree
	# in target/kernel-build survives, so the rebuild is incremental, and
	# so does dist/rootfs.ext4 -- a config change to the kernel is no
	# reason to rebuild busybox and the rootfs image too.
	rm -f $(DIST)/bzImage
	echo "cella: removed $(DIST)/bzImage -- next 'make dist' rebuilds the kernel"

# --- Probes ---------------------------------------------------------
#
# Standalone diagnostics for questions too fiddly (or too dependent on
# real hardware/kernel behavior) to safely resolve from documentation
# or code-reading alone -- each is a real, self-contained program
# (probes/<name>/) that exercises real KVM ioctls or a real cella boot
# and reports what actually happens, not what should happen. NOT part
# of `make test`/`make smoke`: these are one-off investigation tools
# for chasing a specific bug, not routine regression checks, and some
# are expected to legitimately FAIL until the bug they're chasing is
# fixed -- that's the point, a concrete measurement instead of a guess.
# Add a new one under probes/<name>/ (its own Cargo.toml + src/main.rs)
# and a target here when the next one of these comes up.

# Parameters for the probe targets. Each value below is the default that
# cella uses. Every one is `?=`, therefore an environment variable or an
# assignment on the command line takes precedence:
#
#   make probe-freeze-thaw-clock CELLA_FROZEN_SECS=45
#   CELLA_EXTRA_CMDLINE="tsc=nowatchdog" make probe-freeze-thaw-clock
#
# CELLA_FROZEN_SECS
#     probe-freeze-thaw-clock. The length of the freeze, in real seconds.
#     The default of 6 is several times the heartbeat period of 1 s, thus
#     a guest that let real time enter shows an obvious jump. Measurement
#     shows that the error at the thaw does not change with this value, so
#     a longer freeze gives no more information. Use 0 to thaw at once.
#
# CELLA_POST_THAW_SECS
#     probe-freeze-thaw-clock. The length of the measurement of the clock
#     rate after the thaw, in real seconds. The probe fits the monotonic
#     clock of the guest against the clock of the host over this period.
#     /proc/uptime has a resolution of 10 ms, therefore 30 s gives a
#     resolution of approximately 350 ppm. A shorter period gives a worse
#     resolution. Use 0 to omit this measurement.
#
# CELLA_TIME_ARGS
#     probe-freeze-thaw-clock and probe-wallclock. The time arguments on
#     the kernel command line. The default is the default of cella. Set it
#     to a different value to compare the options, or to an empty string
#     to run without any of them.
#
#     cella rewinds the TSC of the guest at every thaw. The guest must
#     therefore not use the TSC as a monotonic reference, and it must not
#     compare the TSC against kvm-clock. These values were measured, each
#     with a freeze of 6 real seconds:
#
#     ""  (no time arguments)
#         The clocksource watchdog reports a skew of 5 ms to 27 ms after
#         every thaw and marks the TSC unstable. Time stays correct for
#         the guest, but the kernel reports a fault.
#
#     "tsc=unstable clocksource=kvm-clock"
#         No fault after a thaw. The guest never uses the TSC for
#         timekeeping. The kernel prints one line at boot: "tsc: Marking
#         TSC unstable due to boot parameter". This statement is true for
#         cella, because cella rewinds the TSC.
#
#     "tsc=nowatchdog clocksource=kvm-clock"
#         No fault after a thaw, and no line at boot. The TSC stays a
#         candidate clocksource but the kernel does not verify it.
#         clocksource=kvm-clock keeps kvm-clock selected.
#
#     "tsc=reliable clocksource=kvm-clock"   (the default of cella)
#         No fault after a thaw, and no line at boot. Note the limit of
#         this value: it tells the guest that the TSC is a reliable
#         timeline, and that is not true across a thaw. The guest is
#         protected because clocksource=kvm-clock keeps kvm-clock
#         selected, so the guest does not read the TSC for timekeeping.
#
#     One line appears at boot with every value above: "Unstable clock
#     detected, switching default tracing clock". It comes from
#     PVCLOCK_TSC_STABLE_BIT, which is absent in the pvclock page. The
#     host KVM owns that bit and sets it only when the TSC of the host is
#     stable. This host is a virtual machine and reports "TSCs
#     unsynchronized", therefore no guest argument can change that line.
#
# CELLA_EXTRA_CMDLINE
#     probe-freeze-thaw-clock. Text to append to the kernel command line,
#     after the time arguments. The default is empty. Use it for
#     arguments that are not about time.
#
# CELLA_OBSERVE_SECS
#     probe-wallclock. The length of the control test, in real seconds.
#     The probe keeps the guest running for this period with no freeze,
#     and reports the clock errors of the kernel. This is the control for
#     probe-freeze-thaw-clock: it shows whether an error comes from the
#     freeze or from the host. The clocksource watchdog runs twice per
#     second, thus the default of 60 gives approximately 120 rounds. Use
#     0 to omit the control test.
#
# All probes also accept CELLA_BIN, CELLA_TEST_KERNEL, CELLA_TEST_DISK,
# and CELLA_TEST_TAP, with the same meaning as in the smoke tests.
CELLA_FROZEN_SECS ?= 6
CELLA_POST_THAW_SECS ?= 30
CELLA_TIME_ARGS ?= tsc=reliable clocksource=kvm-clock
CELLA_EXTRA_CMDLINE ?=
CELLA_OBSERVE_SECS ?= 60
export CELLA_FROZEN_SECS CELLA_POST_THAW_SECS CELLA_TIME_ARGS CELLA_EXTRA_CMDLINE
export CELLA_OBSERVE_SECS

probe-sregs: ## KVM_SET_SREGS ordering: does CS.L=1 need CR0.PG/EFER.LMA set in the *same* ioctl call? (no /dev/kvm needed beyond opening it; boots nothing, just exercises raw ioctls -- see probes/sregs/src/main.rs)
	$(LOG)
	$(CARGO) run --manifest-path probes/sregs/Cargo.toml

probe-wallclock: build dist ## Does the guest's wall-clock land near real time at boot, with no RTC device? (needs /dev/kvm + tap0; see probes/wallclock/src/main.rs)
	$(LOG)
	$(CARGO) run --manifest-path probes/wallclock/Cargo.toml

probe-freeze-thaw-clock: build dist ## Does freeze/thaw leak real elapsed time into the guest's clock? (needs /dev/kvm + tap0, takes ~15s; see probes/freeze-thaw-clock/src/main.rs)
	$(LOG)
	$(CARGO) run --manifest-path probes/freeze-thaw-clock/Cargo.toml
