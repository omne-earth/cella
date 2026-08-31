SHELL := /bin/bash
.ONESHELL:
.SHELLFLAGS := -ueo pipefail -c
LOGDIR := .logs
LOG = @mkdir -p $(LOGDIR); CELLA_LOG_FILE="$(LOGDIR)/$(subst /,_,$@)-$$(date +%Y%m%d-%H%M%S).log"; exec > >(tee -a "$$CELLA_LOG_FILE") 2>&1; echo "=== make $@ -- $$(date -Is) ==="

CARGO ?= cargo
SCRIPTS := scripts
DIST := dist

# Local overrides, not committed -- copy .env.example to .env to change these.
-include .env
CELLA_TAP ?= tap0
CELLA_TAP_CIDR ?= 192.168.200.1/24

# https://www.kernel.org/releases.json.
KERNEL_VERSION ?= 7.2.2
BUSYBOX_VERSION ?= 1.37.0
export KERNEL_VERSION BUSYBOX_VERSION

.PHONY: help build build-static debug check lint fmt fmt-check \
        unit-test integration-test selftest test test-all \
        init dist dist-nested setup-tap \
        boot enter freeze thaw remove demo smoke smoke-boot smoke-thaw smoke-net smoke-nested-boot smoke-nested-boot-airgapped smoke-nested-boot-hybrid smoke-nested-boot-www smoke-clean test-jail test-seccomp \
        clean distclean distclean-kernel distclean-rootfs logs-clean lines \
        probe-sregs probe-wallclock probe-freeze-thaw-clock probe-prefault-ept probe-thaw-gate probe-inception \
        kernel-config-check

help: ## Show this help
	$(LOG)
	echo "cella -- build, lint, and test targets"
	echo ""
	echo "Build:"
	grep -hE '^(build|build-static|debug|check|lint|fmt|fmt-check):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Tests that need no /dev/kvm (unit + integration, run anywhere):"
	grep -hE '^(unit-test|integration-test|selftest|test|test-jail|test-seccomp):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Run: a real jailed guest, interactively:"
	grep -hE '^(boot|enter|freeze|thaw|remove|demo):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Smoke tests: real KVM, a real guest (one target per workflow):"
	grep -hE '^(smoke|smoke-boot|smoke-thaw|smoke-net|smoke-nested-boot|smoke-nested-boot-airgapped|smoke-nested-boot-hybrid|smoke-nested-boot-www|smoke-clean):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Setup:"
	grep -hE '^(init|dist|dist-nested|setup-tap|kernel-config-check):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Everything:"
	grep -hE '^(test-all|clean|distclean|distclean-kernel|distclean-rootfs|logs-clean|lines):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Probes: diagnostics, run by hand (smoke-thaw runs the freeze/thaw one):"
	grep -hE '^(probe-sregs|probe-wallclock|probe-freeze-thaw-clock|probe-prefault-ept|probe-thaw-gate|probe-inception):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true

# --- Build ------------------------------------------------------------

build: ## Release build (target/release/cella)
	$(LOG)
	$(CARGO) build --release

debug: ## Debug build (target/debug/cella), faster to compile
	$(LOG)
	$(CARGO) build

# Sentinel with real input dependencies: the static binaries rebuild
# when a source file changes, and a stale binary can no longer ride
# into a rootfs image unnoticed.
.static: $(shell find src -name '*.rs') Cargo.toml Cargo.lock \
         probes/freeze-thaw-clock/src/main.rs probes/freeze-thaw-clock/Cargo.toml \
         $(SCRIPTS)/build/static.sh | .toolbox
	$(LOG)
	$(SCRIPTS)/build/static.sh
	touch .static

build-static: .static ## Static cella + probe for the nested rootfs, built inside the toolbox (mtime-tracked via .static)

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
# These run in an ordinary container. `make unit-test` stays fast during
# work on the code. `make test` runs everything in this section.

unit-test: ## cargo test --lib (inline #[cfg(test)] modules)
	$(LOG)
	$(CARGO) test --lib

integration-test: ## cargo test --tests (tests/*.rs, real virtio-mmio/blk logic, no KVM)
	$(LOG)
	$(CARGO) test --tests

selftest: build ## Sanity-run the seccomp self-test binary directly (see also: make test-seccomp)
	$(LOG)
	# This binary must exit 159 (SIGSYS). `set -e` stops a recipe at such
	# an exit, thus the code takes the status with `|| status=$$?`.
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

# --- Run: a jailed guest in the foreground ---------------------------

# The guest address on the TAP subnet, for the in-kernel ip= config.
CELLA_GUEST_IP ?= 192.168.200.2
# The state directory of the guest. One directory is one guest.
VM_DIR ?= vm1
# NET=none boots without the TAP. A persistent TAP admits one guest at
# a time, thus a second concurrent guest must run without the network.
NET ?= tap
# DIAG=1 adds cella_diag to the kernel command line: the interactive
# image then prints its heartbeat and its diagnostic listings on the
# console. The demo needs them; an interactive session does not.
DIAG ?= 0
# ROOT=ro mounts the root filesystem read-only. A guest that must
# survive a freeze and a thaw needs this today: the freeze does not
# save the virtio device state, and the first post-thaw disk write
# hangs (see docs/FREEZE-THAW.md, "Next steps: virtio state").
ROOT ?= rw

$(DIST)/rootfs-cella.ext4: $(SCRIPTS)/build/rootfs-cella.sh $(SCRIPTS)/build/assets-cella.sh $(DIST)/rootfs.ext4 | .toolbox
	$(LOG)
	rm -f $@
	$(SCRIPTS)/build/assets-cella.sh

boot: build dist $(DIST)/rootfs-cella.ext4 ## Boot a detached jailed guest at $(VM_DIR) -- or thaw it. Attach: make enter. Console log: .logs/
	$(LOG)
	@if ! command -v tmux >/dev/null; then echo "cella: tmux not found -- run: make init"; exit 1; fi
	if tmux has-session -t "cella-$(VM_DIR)" 2>/dev/null; then \
		echo "cella: a guest already runs at $(VM_DIR) -- attach with: make enter"; exit 1; fi
	mkdir -p $(VM_DIR)
	# A guest owns its disk. The first boot copies the interactive
	# image (rootfs-cella, the latest cella mvp image) into the state
	# directory; the jail binds dist/ read-only.
	[ -f $(VM_DIR)/disk.img ] || cp dist/rootfs-cella.ext4 $(VM_DIR)/disk.img
	HOST_IP="$(CELLA_TAP_CIDR)"; HOST_IP="$${HOST_IP%%/*}"
	if [ "$(NET)" = none ]; then
		TAPARGS=""
		CMD="$$(./target/release/cella --print-default-cmdline) root=/dev/vda $(ROOT) virtio_mmio.device=4K@0xd0000000:5"
	else
		TAPARGS="--tap $(CELLA_TAP)"
		CMD="$$(./target/release/cella --print-default-cmdline) root=/dev/vda $(ROOT) virtio_mmio.device=4K@0xd0000000:5 virtio_mmio.device=4K@0xd0001000:6 ip=$(CELLA_GUEST_IP)::$$HOST_IP:255.255.255.0::eth0:off"
	fi
	[ "$(DIAG)" = 1 ] && CMD="$$CMD cella_diag" || true
	# Detached: the guest runs in a tmux session, and the pane is the
	# serial console. pipe-pane mirrors the console into .logs/.
	tmux new-session -d -s "cella-$(VM_DIR)" \
		"$(SCRIPTS)/jail.sh --state-dir $(VM_DIR) --kernel dist/bzImage --disk $(VM_DIR)/disk.img $$TAPARGS --mem-mb 256 --cmdline '$$CMD'"
	tmux pipe-pane -t "cella-$(VM_DIR)" -o "cat >> $(LOGDIR)/console-$(VM_DIR)-$$(date +%Y%m%d-%H%M%S).log"
	# Do not report a running guest before the guest survives its start:
	# a stale sidecar or a busy TAP kills it within the first second.
	sleep 1
	if ! tmux has-session -t "cella-$(VM_DIR)" 2>/dev/null; then
		echo "cella: the guest exited at start -- last console lines:"
		tail -n 5 $$(ls -t $(LOGDIR)/console-$(VM_DIR)-* | head -1)
		exit 1
	fi
	echo "cella: guest running detached at $(VM_DIR)"
	echo "cella: attach:  make enter    (detach again: Ctrl-b d)"
	echo "cella: freeze:  make freeze   thaw: make thaw"

enter: ## Attach to the console of the running guest at $(VM_DIR) (detach: Ctrl-b d)
	@tmux has-session -t "cella-$(VM_DIR)" 2>/dev/null \
		|| { echo "cella: no running guest at $(VM_DIR) -- run: make boot (or: make thaw)"; exit 1; }
	@tmux attach -t "cella-$(VM_DIR)"

thaw: ## Thaw the frozen guest at $(VM_DIR), detached (fails when no frozen state exists; boot also thaws)
	$(LOG)
	[ -f $(VM_DIR)/state ] || { echo "cella: no frozen state in $(VM_DIR) -- run: make boot, then: make freeze"; exit 1; }
	$(MAKE) boot

remove: ## Discard the guest at $(VM_DIR): end its session and delete its state directory
	$(LOG)
	tmux kill-session -t "cella-$(VM_DIR)" 2>/dev/null \
		&& echo "cella: ended the session of $(VM_DIR)" \
		|| echo "cella: no running guest at $(VM_DIR)"
	rm -rf $(VM_DIR)
	echo "cella: removed $(VM_DIR)"

freeze: ## Freeze the running guest (SIGUSR1); thaw it with: make thaw
	$(LOG)
	# -x matches the process name exactly. A -f pattern would match the
	# recipe shell itself, whose command line contains the same text.
	pkill -USR1 -x cella \
		&& echo "cella: freeze signal sent -- the process exits once the state file is written" \
		|| { echo "cella: no running cella process"; exit 1; }

demo: build dist $(DIST)/rootfs-cella.ext4 ## End-to-end demonstration: boot a shell, store a value, freeze, thaw, read the value back. Tears down after.
	$(LOG)
	$(SCRIPTS)/test/demo.sh

# --- Smoke tests: required real KVM ---------------

smoke-boot: build dist ## Boot a real kernel under KVM all the way to a running init (scripts/test/boot.sh)
	$(LOG)
	$(SCRIPTS)/test/boot.sh

smoke-thaw: build dist ## Boot -> freeze (SIGUSR1) -> verify sidecar -> thaw -> one-shot check, then the clock probe
	$(LOG)
	$(SCRIPTS)/test/thaw.sh
	# The script cannot see whether the guest keeps its time. A guest that
	# resumed with a dead timer passed the script in every run.
	#
	# probe-wallclock runs first. It checks the clock of the guest at
	# boot, with no freeze. A failure there means the guest cannot keep
	# time at all, and the result of the second probe would not be
	# meaningful.
	#
	# CELLA_OBSERVE_SECS=0 and CELLA_POST_THAW_SECS=0 leave out the two
	# long measurements. Run the probes by hand for those.
	$(MAKE) probe-wallclock CELLA_OBSERVE_SECS=0 PROBE_CARGO_FLAGS=--release
	$(MAKE) probe-freeze-thaw-clock CELLA_POST_THAW_SECS=0 PROBE_CARGO_FLAGS=--release

# Deliberately not dist-nested: at test time only the artifacts matter,
# and the bare-metal machine receives them through the shared copy.
# Build them with: make dist-nested (needs the toolbox).
smoke-nested-boot-airgapped: build ## cella hosts cella, no network on either layer
	$(LOG)
	$(SCRIPTS)/test/nested-boot.sh airgapped

smoke-nested-boot-hybrid: build ## cella hosts cella, the outer guest networked, the inner airgapped
	$(LOG)
	$(SCRIPTS)/test/nested-boot.sh hybrid

smoke-nested-boot-www: build ## cella hosts cella, both layers networked (the outer init pings the inner guest)
	$(LOG)
	$(SCRIPTS)/test/nested-boot.sh www

smoke-nested-boot: smoke-nested-boot-airgapped smoke-nested-boot-hybrid smoke-nested-boot-www ## All three nested variants

smoke-net: build dist ## Guest answers ICMP over the TAP after boot (scripts/test/net.sh, best-effort)
	$(LOG)
	$(SCRIPTS)/test/net.sh

smoke: smoke-boot smoke-thaw smoke-net smoke-nested-boot probe-inception ## All smoke-* targets + the deep clock probe (skips gracefully without KVM)
	$(LOG)
	echo ""
	echo "=== make smoke: done (see above for any SKIPs) ==="

smoke-clean: ## Kill any stray cella process left running by an interrupted smoke test
	$(LOG)
	# -x matches the process name exactly. A -f pattern kills any
	# invoker whose own command line mentions the binary path.
	pkill -x cella && echo "cella: killed stray process(es)" || echo "cella: nothing to clean up"

# --- Setup --------------------------------------------------------------

.toolbox: $(SCRIPTS)/build/toolbox.sh ## Sentinel: creates + provisions the cella-build toolbox (kernel build toolchain lives there, not on the host)
	$(LOG)
	$(SCRIPTS)/build/toolbox.sh
	touch .toolbox

init: ## One-time host setup (Fedora): installs runtime deps, provisions the build toolbox, creates tap0, builds dist, checks /dev/kvm (needs sudo)
	$(LOG)
	$(SCRIPTS)/setup/bootstrap.sh
	$(MAKE) .toolbox
	$(MAKE) setup-tap
	$(MAKE) dist

# Each artifact names its real inputs. A change to a fragment or an
# init script makes the artifact stale, and the recipe removes the one
# stale file before the build script runs: the script rebuilds what is
# missing and skips the rest.
$(DIST)/bzImage: $(SCRIPTS)/build/kernel-fragment.config $(SCRIPTS)/build/kernel-config-check.sh $(SCRIPTS)/build/assets.sh | .toolbox
	$(LOG)
	rm -f $@
	$(SCRIPTS)/build/assets.sh

$(DIST)/rootfs.ext4: $(SCRIPTS)/build/rootfs.sh $(SCRIPTS)/build/busybox-fragment.config $(SCRIPTS)/build/assets.sh | .toolbox
	$(LOG)
	rm -f $@
	$(SCRIPTS)/build/assets.sh

dist: $(DIST)/bzImage $(DIST)/rootfs.ext4 ## Build a minimal rootfs + bzImage kernel from source (compiled inside the toolbox), skipped if already built

$(DIST)/bzImage-nested: $(SCRIPTS)/build/kernel-fragment.config $(SCRIPTS)/build/kernel-fragment-nested.config $(SCRIPTS)/build/assets-nested.sh | .toolbox
	$(LOG)
	rm -f $@
	$(SCRIPTS)/build/assets-nested.sh

$(DIST)/rootfs-nested.ext4: .static $(SCRIPTS)/build/rootfs-nested.sh $(SCRIPTS)/build/assets-nested.sh $(DIST)/bzImage $(DIST)/rootfs.ext4 | .toolbox
	$(LOG)
	rm -f $@
	$(SCRIPTS)/build/assets-nested.sh

$(DIST)/rootfs-inception.ext4: .static $(SCRIPTS)/build/rootfs-nested.sh $(SCRIPTS)/build/rootfs-inception.sh $(SCRIPTS)/build/assets-nested.sh $(DIST)/bzImage $(DIST)/rootfs.ext4 | .toolbox
	$(LOG)
	rm -f $@
	$(SCRIPTS)/build/assets-nested.sh

dist-nested: dist $(DIST)/bzImage-nested $(DIST)/rootfs-nested.ext4 $(DIST)/rootfs-inception.ext4 ## Nested test assets: bzImage-nested (KVM host stack), rootfs-nested.ext4 (static cella + canonical inner assets), rootfs-inception.ext4 (+ the static probe)

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

logs-clean: ## Delete the run logs in .logs/, and keep the newest one for each target
	$(LOG)
	cd $(LOGDIR)
	keep=$$(ls -1 *.log 2>/dev/null | sort | awk '{ t = $$0; sub(/-[0-9]{8}-[0-9]{6}\.log$$/, "", t); newest[t] = $$0 } END { for (k in newest) print newest[k] }')
	deleted=0
	own=$$(basename "$$CELLA_LOG_FILE")
	for f in $$(ls -1 *.log 2>/dev/null); do
		if [ "$$f" = "$$own" ]; then
			continue
		fi
		case " $$(echo $$keep) " in
		*" $$f "*) ;;
		*) rm -f "$$f"; deleted=$$((deleted + 1)) ;;
		esac
	done
	kept=$$(echo "$$keep" | grep -c . || true)
	echo "cella: kept $$kept log(s), one for each target, and deleted $$deleted older log(s)"

clean: ## cargo clean
	$(LOG)
	$(CARGO) clean

distclean: clean ## clean + remove built dist/ assets
	$(LOG)
	rm -rf $(DIST)

distclean-rootfs: ## Remove just dist/rootfs.ext4, so the next `make dist` rebuilds the rootfs (for a rootfs.sh change)
	$(LOG)
	# The busybox build in target/rootfs-build survives, thus the rebuild
	# only assembles the image.
	rm -f $(DIST)/rootfs.ext4
	echo "cella: removed $(DIST)/rootfs.ext4 -- next 'make dist' rebuilds the rootfs"

distclean-kernel: ## Remove just dist/bzImage, so the next `make dist` rebuilds the kernel (for a kernel-fragment.config change)
	$(LOG)
	rm -f $(DIST)/bzImage
	echo "cella: removed $(DIST)/bzImage -- next 'make dist' rebuilds the kernel"

# --- Probes ---------------------------------------------------------
#
# To add a probe, make probes/<name>/ with its own Cargo.toml and
# src/main.rs, and add a target here.
#
# Parameters. Each is `?=`, thus the environment or the command line
# takes precedence:
#
#   make probe-freeze-thaw-clock CELLA_FROZEN_SECS=45
#
# CELLA_FROZEN_SECS      6    the length of the freeze, in real seconds.
#                             The error at the thaw does not change with
#                             this value. Use 0 to thaw at once.
# CELLA_POST_THAW_SECS   30   the length of the measurement of the clock
#                             rate. /proc/uptime has a resolution of
#                             10 ms, thus 30 s resolves 350 ppm. Use 0 to
#                             leave the measurement out.
# CELLA_OBSERVE_SECS     60   probe-wallclock. The length of the control
#                             test, which runs the guest with no freeze.
#                             The watchdog runs twice per second. Use 0
#                             to leave the control test out.
# CELLA_TIME_ARGS             the time arguments on the command line. An
#                             unset or empty value uses the default of
#                             cella, in src/config.rs. The word "none"
#                             runs the guest with no time arguments.
# CELLA_EXTRA_CMDLINE    ""   more arguments, after the time arguments.
#
# The probes also accept CELLA_BIN, CELLA_TEST_KERNEL, CELLA_TEST_DISK,
# and CELLA_TEST_TAP, as the smoke tests do.
#
# Measured values for CELLA_TIME_ARGS, each with a freeze of 6 seconds.
# cella rewinds the TSC at every thaw, thus the guest must not use the
# TSC as a monotonic reference, and must not compare it against
# kvm-clock:
#
#   ""                                       the watchdog reports a skew
#                                            of 5 ms to 27 ms after each
#                                            thaw
#   "tsc=reliable clocksource=kvm-clock trace_clock=local"
#                                            the default. No fault, and
#                                            no clock message at boot.
#                                            tsc=reliable states that the
#                                            TSC is reliable, which is
#                                            not true across a thaw. The
#                                            guest does not act on it,
#                                            because clocksource=kvm-clock
#                                            keeps kvm-clock selected.
#   "tsc=unstable clocksource=kvm-clock"     no fault. One line at boot:
#                                            "Marking TSC unstable due to
#                                            boot parameter".
#   "tsc=nowatchdog clocksource=kvm-clock"   no fault, and no line at
#                                            boot. Makes no claim about
#                                            the TSC.
#
# Without trace_clock=local the guest also prints "Unstable clock
# detected, switching default tracing clock". That follows from the
# absence of PVCLOCK_TSC_STABLE_BIT, which the host KVM owns and sets
# only when the TSC of the host is stable.

# Flags for the probe builds. A hand run uses the dev profile, which
# compiles fast. smoke-thaw passes --release.
PROBE_CARGO_FLAGS ?=
CELLA_FROZEN_SECS ?= 6
CELLA_POST_THAW_SECS ?= 30
CELLA_TIME_ARGS ?=
CELLA_EXTRA_CMDLINE ?=
CELLA_OBSERVE_SECS ?= 60
export CELLA_FROZEN_SECS CELLA_POST_THAW_SECS CELLA_TIME_ARGS CELLA_EXTRA_CMDLINE
export CELLA_OBSERVE_SECS

probe-sregs: ## KVM_SET_SREGS ordering: does CS.L=1 need CR0.PG/EFER.LMA set in the *same* ioctl call? (no /dev/kvm needed beyond opening it; boots nothing, just exercises raw ioctls -- see probes/sregs/src/main.rs)
	$(LOG)
	$(CARGO) run $(PROBE_CARGO_FLAGS) --manifest-path probes/sregs/Cargo.toml

probe-wallclock: build dist ## Does the guest's wall-clock land near real time at boot, with no RTC device? (needs /dev/kvm + tap0; see probes/wallclock/src/main.rs)
	$(LOG)
	$(CARGO) run $(PROBE_CARGO_FLAGS) --manifest-path probes/wallclock/Cargo.toml

probe-freeze-thaw-clock: build dist ## Does freeze/thaw leak real elapsed time into the guest's clock? (needs /dev/kvm + tap0, takes ~15s; see probes/freeze-thaw-clock/src/main.rs)
	$(LOG)
	$(CARGO) run $(PROBE_CARGO_FLAGS) --manifest-path probes/freeze-thaw-clock/Cargo.toml

# Findings from the 2026-08-30 investigation of the thaw delay:
# - The excess across the freeze is a constant cost of each thaw. It does
#   not change with the length of the freeze (0 s, 6 s, and 20 s all give
#   +23 ms to +28 ms). It is not a clock leak. The save and restore of
#   the TSC and the kvmclock agree to less than 3 us.
# - Cause: a thaw makes a new KVM VM with empty stage-2 page tables. The
#   first heartbeat cycle of the guest takes a stage-2 fault for each
#   page that it touches. The guest runs during these faults, thus its
#   clock counts them.
# - KVM_PRE_FAULT_MEMORY (Linux 6.11+) fills the stage-2 tables before
#   the clock restore. The cost then falls outside the clock window of
#   the guest. The excess decreases from ~25 ms to ~4 ms.
# - Measured difference against the mean of the baseline heartbeat
#   intervals, with the prefill on (2026-08-30). The gate is a 3-sigma
#   prediction interval, |difference| <= 3 * s * sqrt(1 + 1/n), with s
#   the sample standard deviation of the n baseline intervals
#   (~ +/-1.2 ms to +/-1.7 ms in these runs):
#     nested KVM:  +2.5 ms to +4.3 ms  -> FAIL, outside the interval
#     bare metal:  -0.128 ms           -> PASS, inside the interval
#   The nested KVM remainder comes from the outer hypervisor: a thaw
#   makes a new VM, and the outer hypervisor rebuilds its shadow of the
#   stage-2 tables on the first guest access. No ioctl reaches that
#   shadow; a real guest access does. The warming stub (src/warm.rs,
#   thaw mode "deep", the default) performs those accesses before the
#   clock restore, and the gate then passes on both machines. See
#   docs/NESTED-BOOT.md, "The fix".
# - The probe measures wake-up lateness after the thaw, not a clock step.
#   A clock step smaller than the remaining sleep does not show in the
#   crossing interval, because the wake-up is scheduled in the same clock.
probe-prefault-ept: build dist ## probe-freeze-thaw-clock with the stage-2 prefault at thaw (CELLA_THAW_PREFAULT=ept)
	$(LOG)
	CELLA_THAW_PREFAULT=ept $(CARGO) run --release --manifest-path probes/freeze-thaw-clock/Cargo.toml

# Deliberately not dist-nested: see smoke-nested-boot.
probe-inception: build ## The freeze and thaw clock probe one layer deep: cella freezes and thaws a guest inside a cella guest
	$(LOG)
	$(SCRIPTS)/test/inception.sh

probe-thaw-gate: build dist ## Watch the thawed guest for 30 s: any kernel complaint (watchdog, unstable, oops) is a FAIL
	$(LOG)
	CELLA_POST_THAW_SECS=30 $(CARGO) run --release --manifest-path probes/freeze-thaw-clock/Cargo.toml
