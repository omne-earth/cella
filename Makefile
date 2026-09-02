SHELL := /bin/bash
.ONESHELL:
.SHELLFLAGS := -ueo pipefail -c
LOGDIR := .logs
LOG = @mkdir -p $(LOGDIR); CELLA_LOG_FILE="$(LOGDIR)/$(subst /,_,$@)-$$(date +%Y%m%d-%H%M%S).log"; exec > >(tee -a "$$CELLA_LOG_FILE") 2>&1; echo "=== make $@ -- $$(date -Is) ==="

CARGO ?= cargo
SCRIPTS := scripts

# Local overrides, not committed -- copy .env.example to .env to change these.
-include .env
CELLA_TAP ?= tap0
CELLA_TAP_CIDR ?= 192.168.200.1/24
TAPS ?= 4

# https://www.kernel.org/releases.json.
KERNEL_VERSION ?= 7.2.2
BUSYBOX_VERSION ?= 1.37.0
# Not BASH_VERSION: bash itself defines that variable in every shell,
# and it would win over this default through the environment.
GUEST_BASH_VERSION ?= 5.3
export KERNEL_VERSION BUSYBOX_VERSION GUEST_BASH_VERSION

.PHONY: help build build-smoke install-release install-debug debug check lint fmt fmt-check \
        unit-test integration-test selftest test test-all \
        init golden golden-nested setup-tap \
        boot enter freeze thaw remove doctor smoke smoke-shell smoke-boot smoke-thaw smoke-nested-boot smoke-nested-boot-airgapped smoke-nested-boot-hybrid smoke-nested-boot-www smoke-machine smoke-clean smoke-gateway smoke-gateway-cli smoke-ping smoke-udp smoke-multinet smoke-universe smoke-ledger smoke-device-state device-state-ac1 device-state-ac2 device-state-ac3 device-state-ac4 test-jail test-seccomp test-machine test-one-door \
        clean distclean logs-clean lines \
        probe-sregs probe-wallclock probe-freeze-thaw-clock probe-prefault-ept probe-thaw-gate probe-inception \
        kernel-config-check

help: ## Show this help
	$(LOG)
	echo "cella -- build, lint, and test targets"
	echo ""
	echo "Build:"
	grep -hE '^(build|build-smoke|install-release|install-debug|debug|check|lint|fmt|fmt-check):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Tests that need no /dev/kvm (unit + integration, run anywhere):"
	grep -hE '^(unit-test|integration-test|selftest|test|test-jail|test-seccomp|test-machine|test-one-door):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Run: a real jailed guest, interactively:"
	grep -hE '^(boot|enter|freeze|thaw|remove):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Smoke tests: real KVM, a real guest (one target per workflow):"
	grep -hE '^(smoke|smoke-shell|smoke-boot|smoke-thaw|smoke-ping|smoke-udp|smoke-nested-boot|smoke-nested-boot-airgapped|smoke-nested-boot-hybrid|smoke-nested-boot-www|smoke-machine|smoke-clean|smoke-gateway|smoke-multinet|smoke-universe|smoke-ledger|smoke-device-state|device-state-ac1|device-state-ac2|device-state-ac3|device-state-ac4):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Setup:"
	grep -hE '^(init|golden|golden-nested|setup-tap|doctor|kernel-config-check):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Everything:"
	grep -hE '^(test-all|clean|distclean|logs-clean|lines):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true
	echo ""
	echo "Probes: diagnostics, run by hand (smoke-thaw runs the freeze/thaw one):"
	grep -hE '^(probe-sregs|probe-wallclock|probe-freeze-thaw-clock|probe-prefault-ept|probe-thaw-gate|probe-inception):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t' || true

# --- Build ------------------------------------------------------------

# The development binary, as a real file target: the wrappers depend
# on the file, and cargo runs only when a source changed.
CELLA_DEV := target/release/cella
$(CELLA_DEV): $(shell find src -name '*.rs') Cargo.toml Cargo.lock
	$(LOG)
	$(CARGO) build --release

build: $(CELLA_DEV) ## Release build (target/release/cella) -- the field flavor: no console

# The lab flavor: release-sized, debug-assertions on -- the console
# exists. Every smoke and probe pins to it (see TESTING.md).
CELLA_SMOKE := target/smoke/cella
$(CELLA_SMOKE): $(shell find src -name '*.rs') Cargo.toml Cargo.lock
	$(LOG)
	$(CARGO) build --profile smoke

build-smoke: $(CELLA_SMOKE) ## Lab build (target/smoke/cella): release-sized with the console on

debug: ## Debug build (target/debug/cella), faster to compile
	$(LOG)
	$(CARGO) build

install-release: ## The field flavor: host deps, capabilities, and the console-free binary to ~/.local/bin (scripts/setup/install-release.sh)
	$(LOG)
	$(SCRIPTS)/setup/install-release.sh

install-debug: ## The lab flavor: the smoke-profile binary as cella-debug and its -debug personas (scripts/setup/install-debug.sh)
	$(LOG)
	$(SCRIPTS)/setup/install-debug.sh

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
	$(CELLA_DEV) --selftest-seccomp || status=$$?
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

test-machine: build ## The machine registry verbs against a sandboxed CELLA_HOME (scripts/test/machine.sh)
	$(LOG)
	$(SCRIPTS)/test/machine.sh

test-one-door: ## Static gate: exactly one TX call site writes the TAP (the decision-delivery door)
	$(LOG)
	# With the pass table gone, the TAP is reachable from the decision
	# delivery alone. A second door is a leak path, and it fails the
	# battery here, not a review.
	doors=$$(grep -c 'tap.write_frame' src/devices/virtio/net.rs)
	if [ "$$doors" -ne 1 ]; then
		echo "FAIL: $$doors TX call sites write the TAP (exactly 1 allowed: write_egress)"
		grep -n 'tap.write_frame' src/devices/virtio/net.rs
		exit 1
	fi
	echo "OK: one door -- a single TX call site writes the TAP"

test: check lint unit-test integration-test test-jail test-seccomp test-machine test-one-door ## Everything above: build hygiene + all no-KVM tests
	$(LOG)
	echo ""
	echo "=== make test: all no-KVM checks passed ==="

# --- Run: the machine lifecycle, through the verbs -------------------
#
# The verbs are the interface (see docs/LIFECYCLE.md); these targets
# are convenience wrappers over one default machine. VM names it, and
# CREATE_FLAGS feeds cella create on the first boot.
VM ?= vm1
CREATE_FLAGS ?=

boot: $(CELLA_DEV) ## Create (first time) and run the machine $(VM) -- or thaw it when frozen
	$(LOG)
	$(CELLA_DEV) create $(VM) $(CREATE_FLAGS) 2>/dev/null || true
	if [ -f "$$HOME/.cella/machines/$(VM)/state" ]; then
		$(CELLA_DEV) thaw $(VM)
	else
		$(CELLA_DEV) start $(VM)
	fi
	echo "cella: attach with: make enter (or: cella enter $(VM))"

enter: $(CELLA_DEV) ## Attach to the console of $(VM) (detach: Ctrl-] or exit)
	@$(CELLA_DEV) enter $(VM)

freeze: $(CELLA_DEV) ## Freeze $(VM); resume with: make thaw
	$(LOG)
	$(CELLA_DEV) freeze $(VM)

thaw: $(CELLA_DEV) ## Thaw the frozen machine $(VM)
	$(LOG)
	$(CELLA_DEV) thaw $(VM)

remove: $(CELLA_DEV) ## Discard $(VM): stop it and destroy it
	$(LOG)
	$(CELLA_DEV) stop $(VM) 2>/dev/null || true
	$(CELLA_DEV) destroy $(VM)

# --- Smoke tests: required real KVM ---------------

smoke-shell: build-smoke golden ## A shell learns a value, freezes, thaws, and remembers -- the one gate that drives the machine through enter (scripts/test/shell.sh)
	$(LOG)
	$(SCRIPTS)/test/shell.sh

smoke-boot: build-smoke golden ## Boot a real kernel under KVM all the way to a running init (scripts/test/boot.sh)
	$(LOG)
	$(SCRIPTS)/test/boot.sh

smoke-thaw: build-smoke golden ## Create -> start -> freeze -> verify sidecar -> thaw -> one-shot check, then the clock probe
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
	$(MAKE) probe-wallclock CELLA_OBSERVE_SECS=0
	$(MAKE) probe-freeze-thaw-clock CELLA_POST_THAW_SECS=0

smoke-nested-boot-airgapped: build-smoke golden-nested ## cella hosts cella, no network on either layer
	$(LOG)
	$(SCRIPTS)/test/nested-boot.sh airgapped

smoke-nested-boot-hybrid: build-smoke golden-nested ## cella hosts cella, the outer guest networked, the inner airgapped
	$(LOG)
	$(SCRIPTS)/test/nested-boot.sh hybrid

smoke-nested-boot-www: build-smoke golden-nested ## cella hosts cella, both layers networked (the outer init pings the inner guest)
	$(LOG)
	$(SCRIPTS)/test/nested-boot.sh www

smoke-nested-boot: smoke-nested-boot-airgapped smoke-nested-boot-hybrid smoke-nested-boot-www ## All three nested variants

smoke-machine: $(CELLA_DEV) ## The lifecycle cycle with a real guest: cella selftest (the first migrated target)
	$(LOG)
	$(CELLA_DEV) selftest

smoke-gateway: build-smoke golden ## The gateway ladder: pair wiring (bridge + route), the appliance forwards agent->world, and the pair freezes and thaws together
	$(LOG)
	$(SCRIPTS)/test/gateway.sh

smoke-multinet: build-smoke golden ## A machine takes N taps: two-tap boot, both nics in the guest, host pings eth0, claims exclusive per tap
	$(LOG)
	$(SCRIPTS)/test/multinet.sh

smoke-universe: build-smoke golden ## The universe family end to end: branch (frozen twin, rock to rock), archive (the latch), inspect (evidence at /rock, byte-identical after)
	$(LOG)
	$(SCRIPTS)/test/universe.sh

smoke-udp: build-smoke golden ## No datagram leaves undecided, proven from within the guest: closed drops UDP, open parks it (the park is the freeze), a refusal delivers nothing; the guest's own ICMP lapses the same way (scripts/test/udp.sh)
	$(LOG)
	$(SCRIPTS)/test/udp.sh

smoke-ping: build-smoke golden ## The valve end to end: born closed fails a ping, open parks the reply and freezes, release answers, close darkens again (docs/NETWORK-MODEL.md)
	$(LOG)
	$(SCRIPTS)/test/ping.sh

smoke-gateway-cli: build-smoke golden ## 1.3: close shuts the valve, show lists the hold, release/refuse decide by id prefix, open refuses (docs/NETWORK-MODEL.md)
	$(LOG)
	$(SCRIPTS)/test/gateway-cli.sh

smoke-ledger: build-smoke golden ## 1.2: hold, one fetch parks, the ledger holds one operation with an id and both clocks (docs/NETWORK-MODEL.md)
	$(LOG)
	$(SCRIPTS)/test/ledger.sh

doctor: $(CELLA_DEV) ## Judge the host and the goldens: cella doctor check + verify
	$(LOG)
	$(CELLA_DEV) doctor check
	$(CELLA_DEV) doctor verify

smoke: test smoke-shell smoke-boot smoke-thaw smoke-ping smoke-udp smoke-nested-boot smoke-machine smoke-gateway smoke-gateway-cli smoke-multinet smoke-universe smoke-ledger smoke-device-state probe-inception ## The no-KVM checks first (fail fast), then all smoke-* targets + the deep clock probe (skips gracefully without KVM)
	$(LOG)
	echo ""
	echo "=== make smoke: done (see above for any SKIPs) ==="

# --- Device state across freeze/thaw (docs/DEVICE-STATE.md) ----------
#
# One gate per acceptance criterion, in dependency order. Each gate
# fails until its implementation lands.

device-state-ac1: build-smoke golden ## AC1: the disk survives the thaw -- transport state rides the sidecar (v7); write a file, freeze, thaw, read it back, sync; smoke-shell drops ROOT=ro
	$(LOG)
	$(SCRIPTS)/test/device-state.sh ac1

device-state-ac2: build-smoke golden ## AC2: the network survives the thaw -- the tap claim persists through the manifest, the tap is recreated by convention, and the host pings the guest after a thaw
	$(LOG)
	$(SCRIPTS)/test/device-state.sh ac2

device-state-ac3: build-smoke golden ## AC3: the in-flight layer is exact -- a parked egress frame is delivered and completed after the thaw; the same request works, with no retransmission
	$(LOG)
	$(SCRIPTS)/test/device-state.sh ac3

device-state-ac4: build-smoke golden ## AC4: the verdict is external -- every egress frame parks by default; the test, as the stand-in engine, releases with an allow or freezes, grows the world, and thaws (the world-ratchet gate)
	$(LOG)
	$(SCRIPTS)/test/device-state.sh ac4

smoke-device-state: device-state-ac1 device-state-ac2 device-state-ac3 device-state-ac4 ## All four device-state acceptance gates, in dependency order
	$(LOG)
	echo ""
	echo "=== make smoke-device-state: all gates passed ==="

smoke-clean: ## Kill any stray cella process left running by an interrupted smoke test
	$(LOG)
	# -x matches the process name exactly. A -f pattern kills any
	# invoker whose own command line mentions the binary path.
	# SIGKILL, not SIGTERM: the VMM runs as pid 1 of its namespace
	# (--as-pid-1), and a namespace init ignores a signal without a
	# handler.
	{ pkill -9 -x cella; pkill -9 -x cella-vmm; } && echo "cella: killed stray process(es)" || echo "cella: nothing to clean up"

# --- Setup --------------------------------------------------------------

init: ## One-time host setup (Fedora): deps, toolbox, tap0, and every golden (needs sudo)
	$(LOG)
	$(SCRIPTS)/setup/install.sh
	$(MAKE) setup-tap
	$(MAKE) golden
	$(MAKE) golden-nested

golden: build ## Build the base goldens natively: kernel canonical, rootfs canonical, rootfs cella
	$(LOG)
	$(CELLA_DEV) build kernel canonical
	$(CELLA_DEV) build rootfs canonical
	$(CELLA_DEV) build rootfs cella
	$(CELLA_DEV) build rootfs gateway

golden-nested: build ## Build the nested-family goldens natively: kernel nested, rootfs nested, rootfs inception
	$(LOG)
	$(CELLA_DEV) build kernel nested
	$(CELLA_DEV) build rootfs nested
	$(CELLA_DEV) build rootfs inception

kernel-config-check: ## Resolve kernel-fragment.config against defconfig and report any line kconfig silently overruled (seconds, no compile)
	$(LOG)
	$(SCRIPTS)/build/kernel-config-check.sh

setup-tap: build ## Provision the tap pool + NAT via cella-network (no sudo; make install-release granted the capability)
	$(LOG)
	target/release/cella-network setup --taps $(TAPS)

# --- Everything -----------------------------------------------------

test-all: test golden smoke ## make test, plus every KVM smoke test (skips gracefully without KVM)
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

# --- Probes ---------------------------------------------------------
#
# To add a probe, add a module under src/bin/cella-probe/ and a
# subcommand in its main.rs, and add a target here.
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

CELLA_FROZEN_SECS ?= 6
CELLA_POST_THAW_SECS ?= 30
CELLA_TIME_ARGS ?=
CELLA_EXTRA_CMDLINE ?=
CELLA_OBSERVE_SECS ?= 60
export CELLA_FROZEN_SECS CELLA_POST_THAW_SECS CELLA_TIME_ARGS CELLA_EXTRA_CMDLINE
export CELLA_OBSERVE_SECS

probe-sregs: build ## KVM_SET_SREGS ordering: does CS.L=1 need CR0.PG/EFER.LMA set in the *same* ioctl call? (no /dev/kvm needed beyond opening it; boots nothing -- see src/bin/cella-probe/sregs.rs)
	$(LOG)
	target/smoke/cella-probe sregs

probe-wallclock: build-smoke golden ## Does the guest's wall-clock land near real time at boot, with no RTC device? (needs /dev/kvm + tap0; see src/bin/cella-probe/wallclock.rs)
	$(LOG)
	target/smoke/cella-probe wallclock

probe-freeze-thaw-clock: build-smoke golden ## Does freeze/thaw leak real elapsed time into the guest's clock? (needs /dev/kvm + tap0, takes ~15s; see src/bin/cella-probe/freeze_thaw_clock.rs)
	$(LOG)
	target/smoke/cella-probe freeze-thaw-clock

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
probe-prefault-ept: build-smoke golden ## probe-freeze-thaw-clock with the stage-2 prefault at thaw (CELLA_THAW_PREFAULT=ept)
	$(LOG)
	CELLA_THAW_PREFAULT=ept target/smoke/cella-probe freeze-thaw-clock

probe-inception: build-smoke golden-nested ## The freeze and thaw clock probe one layer deep: cella freezes and thaws a guest inside a cella guest
	$(LOG)
	$(SCRIPTS)/test/inception.sh

probe-thaw-gate: build-smoke golden ## Watch the thawed guest for 30 s: any kernel complaint (watchdog, unstable, oops) is a FAIL
	$(LOG)
	CELLA_POST_THAW_SECS=30 target/smoke/cella-probe freeze-thaw-clock
