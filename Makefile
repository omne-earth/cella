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

.PHONY: help build build-smoke install debug check lint fmt fmt-check \
        unit-test integration-test selftest test test-all \
        init golden golden-nested  \
        boot enter freeze thaw remove doctor \
        smoke smoke-shell smoke-boot smoke-thaw \
        smoke-cella-doctor smoke-cella-vmm smoke-cella-machine \
        smoke-cella-gateway smoke-cella-network smoke-cella-probe \
        smoke-engine engine-w1 engine-w2 engine-w3 engine-w4 engine-w5 \
        smoke-nested-boot smoke-nested-boot-airgapped \
        smoke-nested-boot-hybrid smoke-nested-boot-www \
        smoke-machine smoke-clean smoke-gateway smoke-gateway-cli smoke-wire \
        smoke-world smoke-rootless smoke-translator-port-neg \
        smoke-ping smoke-udp smoke-collide smoke-inspection \
        smoke-witness smoke-multinet smoke-universe smoke-ledger smoke-chain \
        smoke-device-state device-state-ac1 device-state-ac2 \
        device-state-ac3 device-state-ac4 device-state-ac5 \
        test-jail test-seccomp test-seccomp-vmm-kvm test-seccomp-personas \
        test-seccomp-gateway test-seccomp-universe test-seccomp-build \
        test-seccomp-doctor test-seccomp-network test-seccomp-probe \
        test-seccomp-machine \
        test-machine test-one-door test-witness \
        clean distclean logs-clean lines \
        probe-sregs probe-wallclock probe-freeze-thaw-clock \
        probe-prefault-ept probe-thaw-gate probe-inception \
        kernel-config-check

# Help rendering: a target's description sits on the line ABOVE it
# as a "## " comment; this awk pairs the two, filtered by a name
# alternation.
define help_section
awk -v pat='$(1)' 'substr($$0,1,3)=="## "{t=substr($$0,4);c=(c=="")?t:c" "t;next} $$0~/^[A-Za-z0-9_.\-]+:/{n=$$0;sub(/:.*/,"",n);if(c!=""&&n~"^("pat")$$")printf "%s\t%s\n",n,c} {c=""}' $(MAKEFILE_LIST) | sort | column -t -s $$'\t' || true
endef

## Show this help
help:
	$(LOG)
	echo "cella -- build, lint, and test targets"
	echo ""
	echo "Build:"
	$(call help_section,build|build-smoke|install|debug|check|lint|fmt|fmt-check)
	echo ""
	echo "Tests that need no /dev/kvm (unit + integration, run anywhere):"
	$(call help_section,unit-test|integration-test|selftest|test|test-jail|test-seccomp|test-seccomp-vmm-kvm|test-seccomp-personas|test-seccomp-gateway|test-seccomp-universe|test-seccomp-build|test-seccomp-doctor|test-seccomp-network|test-seccomp-probe|test-seccomp-machine|test-machine|test-one-door|test-witness)
	echo ""
	echo "Run: a real jailed guest, interactively:"
	$(call help_section,boot|enter|freeze|thaw|remove)
	echo ""
	echo "Smoke tests: real KVM, a real guest (one target per workflow):"
	$(call help_section,$(SMOKE_ALTERNATION))
	echo ""
	echo "Setup:"
	$(call help_section,init|golden|golden-nested|setup-tap|doctor|kernel-config-check)
	echo ""
	echo "Everything:"
	$(call help_section,test-all|clean|distclean|logs-clean|lines)
	echo ""
	echo "Probes: diagnostics, run by hand (smoke-thaw runs the freeze/thaw one):"
	$(call help_section,probe-sregs|probe-wallclock|probe-freeze-thaw-clock|probe-prefault-ept|probe-thaw-gate|probe-inception)

# The smoke roster, one list: the help section renders it, and the
# alternation below is generated -- a new gate is added here once.
SMOKE_TARGETS := smoke smoke-shell smoke-boot smoke-thaw smoke-ping \
        smoke-udp smoke-collide smoke-inspection smoke-witness \
        smoke-nested-boot \
        smoke-nested-boot-airgapped smoke-nested-boot-hybrid \
        smoke-nested-boot-www smoke-machine smoke-clean \
        smoke-gateway smoke-gateway-cli smoke-wire smoke-world \
        smoke-rootless smoke-translator-port-neg smoke-multinet \
        smoke-universe smoke-ledger smoke-chain \
        smoke-cella-doctor smoke-cella-vmm smoke-cella-machine \
        smoke-cella-gateway smoke-cella-network smoke-cella-probe \
        smoke-engine engine-w1 engine-w2 engine-w3 engine-w4 engine-w5 \
        smoke-device-state device-state-ac1 device-state-ac2 \
        device-state-ac3 device-state-ac4 device-state-ac5
empty :=
space := $(empty) $(empty)
SMOKE_ALTERNATION := $(subst $(space),|,$(strip $(SMOKE_TARGETS)))

# --- Build ------------------------------------------------------------

# The development binary, as a real file target: the wrappers depend
# on the file, and cargo runs only when a source changed.
CELLA_DEV := target/release/cella
$(CELLA_DEV): $(shell find crates -name '*.rs') Cargo.toml Cargo.lock
	$(LOG)
	$(CARGO) build --release

## Release build (target/release/cella) -- the field flavor: no console
build: $(CELLA_DEV)

# The lab flavor: release-sized, debug-assertions on -- the console
# exists. Every smoke and probe pins to it (see TESTING.md).
CELLA_SMOKE := target/smoke/cella
$(CELLA_SMOKE): $(shell find crates -name '*.rs') Cargo.toml Cargo.lock
	$(LOG)
	$(CARGO) build --profile smoke

## Lab build (target/smoke/cella): release-sized with the console on
build-smoke: $(CELLA_SMOKE)

## Debug build (target/debug/cella), faster to compile
debug:
	$(LOG)
	$(CARGO) build

## The field flavor: host deps and the console-free binaries to
## ~/.cella/bin (scripts/setup/install.sh)
install:
	$(LOG)
	$(SCRIPTS)/setup/install.sh


## cargo check, no codegen
check:
	$(LOG)
	$(CARGO) check --all-targets

## cargo clippy (all targets) + fmt-check
lint: fmt-check
	$(LOG)
	$(CARGO) clippy --all-targets

## Apply cargo fmt
fmt:
	$(LOG)
	$(CARGO) fmt

## Verify formatting without changing files (CI-friendly)
fmt-check:
	$(LOG)
	$(CARGO) fmt -- --check

# --- Tests that need no /dev/kvm ---------------------------------------
#
# These run in an ordinary container. `make unit-test` stays fast during
# work on the code. `make test` runs everything in this section.

## cargo test --lib (inline #[cfg(test)] modules)
unit-test:
	$(LOG)
	$(CARGO) test --lib

## cargo test --tests (tests/*.rs, real virtio-mmio/blk logic, no KVM)
integration-test:
	$(LOG)
	$(CARGO) test --tests

## Sanity-run the seccomp self-test binary directly (see also: make test-
## seccomp)
selftest: build
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

## Rootless bwrap jail actually confines the process (scripts/test/jail.sh)
test-jail: build
	$(LOG)
	$(SCRIPTS)/test/jail.sh

## Lane a's gate (1.6.14a): per-machine sub-uid, cross-machine refusal, bind-
## set refusal (scripts/test/jail-identity.sh)
test-jail-identity: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/jail-identity.sh

## The real BPF filter kills a disallowed syscall (scripts/test/seccomp.sh)
test-seccomp: build
	$(LOG)
	$(SCRIPTS)/test/seccomp.sh

## Lane b (1.6.14b): a KVM ioctl request outside the table kills the VMM
## (scripts/test/seccomp-kvm.sh)
test-seccomp-vmm-kvm: build
	$(LOG)
	$(SCRIPTS)/test/seccomp-kvm.sh

## Lane b: cella-gateway's own filter kills a disallowed syscall
test-seccomp-gateway: build
	$(LOG)
	$(SCRIPTS)/test/seccomp-persona.sh cella-gateway

## Lane b: cella-universe's own filter kills a disallowed syscall
test-seccomp-universe: build
	$(LOG)
	$(SCRIPTS)/test/seccomp-persona.sh cella-universe

## Lane b: cella-build's own filter kills a disallowed syscall
test-seccomp-build: build
	$(LOG)
	$(SCRIPTS)/test/seccomp-persona.sh cella-build

## Lane b: cella-doctor's own filter kills a disallowed syscall
test-seccomp-doctor: build
	$(LOG)
	$(SCRIPTS)/test/seccomp-persona.sh cella-doctor

## Lane b: cella-network's own filter kills a disallowed syscall (table not
## installed in production yet -- see cella-network/src/seccomp.rs)
test-seccomp-network: build
	$(LOG)
	$(SCRIPTS)/test/seccomp-persona.sh cella-network

## Lane b: cella-probe's own filter kills a disallowed syscall (table not
## installed in production yet -- see cella-probe/src/seccomp.rs)
test-seccomp-probe: build
	$(LOG)
	$(SCRIPTS)/test/seccomp-persona.sh cella-probe

## Lane b: cella-machine's own filter kills a disallowed syscall
test-seccomp-machine: build
	$(LOG)
	$(SCRIPTS)/test/seccomp-persona.sh cella-machine

## Lane b (1.6.14b): every persona's negative seccomp gate, one binary at a
## time
test-seccomp-personas: test-seccomp test-seccomp-vmm-kvm \
        test-seccomp-gateway test-seccomp-universe test-seccomp-build \
        test-seccomp-doctor test-seccomp-network test-seccomp-probe \
        test-seccomp-machine
	$(LOG)
	echo ""
	echo "=== test-seccomp-personas: every persona's filter kills its own canary ==="

## The machine registry verbs against a sandboxed CELLA_HOME
## (scripts/test/machine.sh)
test-machine: build
	$(LOG)
	$(SCRIPTS)/test/machine.sh

## Static gate: exactly one TX call site writes the edge (the decision-
## delivery door)
test-one-door:
	$(LOG)
	# With the pass table gone, the edge (the translator connection
	# -- 1.6.14e) is reachable from the decision delivery alone. A second door is a leak path, and it fails the battery
	# here, not a review.
	doors=$$(grep -c 'edge.write_frame' crates/cella-vmm/src/devices/virtio/net.rs)
	if [ "$$doors" -ne 1 ]; then
		echo "FAIL: $$doors TX call sites write the edge (exactly 1 allowed: write_egress)"
		grep -n 'edge.write_frame' crates/cella-vmm/src/devices/virtio/net.rs
		exit 1
	fi
	echo "OK: one door -- a single TX call site writes the edge"

## Static gate: one witness door per persona binary (the one-door pattern
## applied to the audit)
test-witness:
	$(LOG)
	# Every persona binary witnesses its own verbs since the split
	# (1.6.13): one audit::witness call site per persona main --
	# machine, gateway, universe, build, doctor, network, engine --
	# shim owns none (it owns no verbs). The VMM is internal (its
	# actions are the border events), and the probes are diagnostics.
	doors=$$(grep -rln 'audit::witness(' crates/*/src/main.rs | wc -l)
	if [ "$$doors" -ne 7 ]; then
		echo "FAIL: $$doors witness doors (exactly 7 persona mains)"
		grep -rln 'audit::witness(' crates/*/src/main.rs
		exit 1
	fi
	if grep -q 'audit::witness(' crates/cella/src/main.rs; then
		echo "FAIL: the shim witnesses -- it owns no verbs"
		exit 1
	fi
	echo "OK: seven witness doors, one per persona; the shim owns none"

## Everything above: build hygiene + all no-KVM tests
test: check lint unit-test integration-test test-jail test-seccomp-personas \
        test-machine test-one-door test-witness
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

## Create (first time) and run the machine $(VM) -- or thaw it when frozen
boot: $(CELLA_DEV)
	$(LOG)
	$(CELLA_DEV) create $(VM) $(CREATE_FLAGS) 2>/dev/null || true
	if [ -f "$$HOME/.cella/machines/$(VM)/state" ]; then
		$(CELLA_DEV) thaw $(VM)
	else
		$(CELLA_DEV) start $(VM)
	fi
	echo "cella: attach with: make enter (or: cella enter $(VM))"

## Attach to the console of $(VM) (detach: Ctrl-] or exit)
enter: $(CELLA_DEV)
	@$(CELLA_DEV) enter $(VM)

## Freeze $(VM); resume with: make thaw
freeze: $(CELLA_DEV)
	$(LOG)
	$(CELLA_DEV) freeze $(VM)

## Thaw the frozen machine $(VM)
thaw: $(CELLA_DEV)
	$(LOG)
	$(CELLA_DEV) thaw $(VM)

## Discard $(VM): stop it and destroy it
remove: $(CELLA_DEV)
	$(LOG)
	$(CELLA_DEV) stop $(VM) 2>/dev/null || true
	$(CELLA_DEV) destroy $(VM)

# --- Smoke tests: required real KVM ---------------

## A shell learns a value, freezes, thaws, and remembers -- the one gate that
## drives the machine through enter (scripts/test/shell.sh)
smoke-shell: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/shell.sh

## Boot a real kernel under KVM all the way to a running init
## (scripts/test/boot.sh)
smoke-boot: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/boot.sh

## Create -> start -> freeze -> verify sidecar -> thaw -> one-shot check, then
## the clock probe
smoke-thaw: build-smoke golden
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

## cella hosts cella, no network on either layer
smoke-nested-boot-airgapped: build-smoke golden-nested
	$(LOG)
	$(SCRIPTS)/test/nested-boot.sh airgapped

## cella hosts cella, the outer guest networked, the inner airgapped
smoke-nested-boot-hybrid: build-smoke golden-nested
	$(LOG)
	$(SCRIPTS)/test/nested-boot.sh hybrid

## cella hosts cella, both layers networked (the outer init pings the inner
## guest)
smoke-nested-boot-www: build-smoke golden-nested
	$(LOG)
	$(SCRIPTS)/test/nested-boot.sh www

## All three nested variants
smoke-nested-boot: smoke-nested-boot-airgapped smoke-nested-boot-hybrid \
        smoke-nested-boot-www

## The lifecycle cycle with a real guest: cella selftest (the first migrated
## target)
smoke-machine: $(CELLA_DEV)
	$(LOG)
	$(CELLA_DEV) selftest

## The gateway ladder: the appliance shape over wires -- the agent reaches
## the world only through the gateway machine, and the pair freezes and
## thaws together
smoke-gateway: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/gateway.sh

## The wire plane (1.6.14e): two machines, one wire, no host object; both
## membranes judge; the frozen peer's mail is discarded and counted
smoke-wire: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/wire.sh

## The world plane, stateless half (1.6.14e): --net world -- ARP and gateway
## echo at the edge, ICMP/UDP through sockets, replies park incoming
smoke-world: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/world.sh

## The rootless sweep (1.6.14e): no capability on any cella binary, no tap,
## bridge, or nft table of cella's on the host, no boot unit
smoke-rootless: build
	$(LOG)
	$(SCRIPTS)/test/rootless.sh

## The tether (negative): a machine dir removed without destroy orphans no
## translator -- the process exits on its own and the knock port frees
smoke-translator-port-neg: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/translator-port-neg.sh

## engine-w1 (docs/WORLD-ENGINE.md, "The gates"): the stream stands --
## the bridge dials the motor, and a park arrives as a
## well-formed Event
engine-w1: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/engine.sh w1

## engine-w2 (docs/WORLD-ENGINE.md, "The gates"): the decision lands --
## the engine's release delivers and its refusal lapses with the why
engine-w2: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/engine.sh w2

## engine-w3 (docs/WORLD-ENGINE.md, "The gates"): stillness on engine
## halt -- the hold waits, nothing defaults, and a restarted engine
## judges it
engine-w3: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/engine.sh w3

## engine-w4 (docs/WORLD-ENGINE.md, "The gates"): the frozen machine --
## decisions stage in the verdict file, the pidless kick stages
## without error, and the thaw applies in park order
engine-w4: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/engine.sh w4

## engine-w5 (docs/WORLD-ENGINE.md, "The gates"): two judges -- the
## operator's hand interleaves with the stream, both witnessed, no
## decision applied twice
engine-w5: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/engine.sh w5

## The world-engine gates, in dependency order
smoke-engine: engine-w1 engine-w2 engine-w3 engine-w4 engine-w5

## A machine takes N nics: a two-nic boot, both present in the guest,
## every crossing decided per nic
smoke-multinet: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/multinet.sh

## The universe family end to end: branch (frozen twin, rock to rock), archive
## (the latch), inspect (evidence at /rock, byte-identical after)
smoke-universe: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/universe.sh

## No datagram leaves undecided, proven from within the guest: closed drops
## UDP, open parks it (the park is the freeze), a refusal delivers nothing;
## the guest's own ICMP lapses the same way (scripts/test/udp.sh)
smoke-udp: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/udp.sh

## Every verb is an event: machine-scoped verbs in machines/<vm>/audit,
## placeless in the root book, uid+gid+persona on each; show twice makes two
## entries; the harvest files and says so (scripts/test/witness.sh)
smoke-witness: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/witness.sh

## Judgment requires sight, sight requires stillness: inspect renders a frozen
## hold's plaintext, seals the sealed, witnesses the look in the chronicle,
## refuses a running machine (scripts/test/inspection.sh)
smoke-inspection: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/inspection.sh

## The matcher never guesses: a thaw over a colliding sidecar re-mints, holds
## every ambiguous frame, delivers none; refused stale ids lapse by the book
## (scripts/test/collide.sh)
smoke-collide: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/collide.sh

## The valve end to end: born closed fails a ping, open parks the reply and
## freezes, release answers, close darkens again (docs/NETWORK-MODEL.md)
smoke-ping: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/ping.sh

## The gateway verbs: born closed asserted, open arms the membrane, show
## lists the hold, release/refuse decide by id prefix, close returns the
## dark (docs/NETWORK-MODEL.md)
smoke-gateway-cli: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/gateway-cli.sh

## 1.2: hold, one fetch parks, the ledger holds one operation with an id and
## both clocks (docs/NETWORK-MODEL.md)
smoke-ledger: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/ledger.sh

## 1.6.14d: field 15 chains both books by SHA-256 of the predecessor's framed
## bytes -- an intact book verifies, a tampered one snaps loudly, a branched
## twin's book forks and stays valid (scripts/test/chain.sh)
smoke-chain: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/chain.sh

## Judge the host and the goldens: cella doctor check + verify
doctor: $(CELLA_DEV)
	$(LOG)
	$(CELLA_DEV) doctor check
	$(CELLA_DEV) doctor verify

# --- The battery, one part per CLI --------------------------------
#
# Each part gathers the gates whose subject is one binary: a red
# part names an accused binary, the same granularity as the
# per-binary jail, seccomp list, and SELinux domain. A gate belongs
# to the verb under test, not the machinery it rides (every gate
# boots a VMM; only cella-vmm's part is ABOUT the VMM).

## cella-doctor's part: the host contract -- the rootless sweep, then doctor
## check + verify
smoke-cella-doctor: smoke-rootless doctor

## cella-vmm's part: boot, the guest console, and device state across
## freeze/thaw
smoke-cella-vmm: smoke-shell smoke-boot smoke-device-state

## cella-machine's part: the lifecycle verbs, thaw, clean, and the nested
## recursion
smoke-cella-machine: smoke-thaw smoke-machine smoke-clean smoke-nested-boot

## cella-gateway's part: the valve, parking, the books, and their chains
smoke-cella-gateway: smoke-ping smoke-udp smoke-collide smoke-gateway \
        smoke-gateway-cli smoke-inspection smoke-ledger smoke-chain

## cella-network's part: the translator planes -- wire, world, multinet, and
## the tether
smoke-cella-network: smoke-wire smoke-world smoke-multinet \
        smoke-translator-port-neg

## cella-probe's part: the witness doors, the universe, and the deep clock
## probe
smoke-cella-probe: smoke-witness smoke-universe probe-inception

## The whole battery: the no-KVM checks first (fail fast), then one part per
## CLI, ground first
smoke: test smoke-cella-doctor smoke-cella-vmm smoke-cella-machine \
        smoke-cella-gateway smoke-cella-network smoke-cella-probe smoke-engine
	$(LOG)
	echo ""
	echo "=== make smoke: done (see above for any SKIPs) ==="

# --- Device state across freeze/thaw (docs/DEVICE-STATE.md) ----------
#
# One gate per acceptance criterion, in dependency order. Each gate
# fails until its implementation lands.

## AC1: the disk survives the thaw -- transport state rides the sidecar (v7);
## write a file, freeze, thaw, read it back, sync; smoke-shell drops ROOT=ro
device-state-ac1: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/device-state.sh ac1

## AC2: the network survives the thaw -- the machine-lifetime translator
## holds the flows across the freeze; the gate exercises the nic across
## freeze and thaw, every answer decided
device-state-ac2: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/device-state.sh ac2

## AC3: the in-flight layer is exact -- a parked egress frame is delivered and
## completed after the thaw; the same request works, with no retransmission
device-state-ac3: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/device-state.sh ac3

## AC4: the verdict is external -- the request toward a world that does not
## exist parks and freezes; the world grows while the machine sleeps; the
## release lands the same request (the world-ratchet gate)
device-state-ac4: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/device-state.sh ac4

## AC5: the true world -- a real internet fetch crosses the total membrane,
## one decision per frame (skips when offline; rides the peer-patience bound)
device-state-ac5: build-smoke golden
	$(LOG)
	$(SCRIPTS)/test/device-state.sh ac5

## The five device-state acceptance gates, in dependency order
smoke-device-state: device-state-ac1 device-state-ac2 device-state-ac3 \
        device-state-ac4 device-state-ac5
	$(LOG)
	echo ""
	echo "=== make smoke-device-state: all gates passed ==="

## Kill any stray cella process left running by an interrupted smoke test
smoke-clean:
	$(LOG)
	# -x matches the process name exactly. A -f pattern kills any
	# invoker whose own command line mentions the binary path.
	# SIGKILL, not SIGTERM: the VMM runs as pid 1 of its namespace
	# (--as-pid-1), and a namespace init ignores a signal without a
	# handler.
	{ pkill -9 -x cella; pkill -9 -x cella-vmm; } && echo "cella: killed stray process(es)" || echo "cella: nothing to clean up"

# --- Setup --------------------------------------------------------------

## One-time host setup (Fedora): deps, toolbox, the sub-id delegation,
## and every golden (the one sudo moment)
init:
	$(LOG)
	$(SCRIPTS)/setup/install.sh
	$(MAKE) 
	$(MAKE) golden
	$(MAKE) golden-nested

## Build the base goldens natively: kernel canonical, rootfs canonical, rootfs
## cella
golden: build
	$(LOG)
	$(CELLA_DEV) build kernel canonical
	$(CELLA_DEV) build rootfs canonical
	$(CELLA_DEV) build rootfs cella
	$(CELLA_DEV) build rootfs gateway

## Build the nested-family goldens natively: kernel nested, rootfs nested,
## rootfs inception
golden-nested: build
	$(LOG)
	$(CELLA_DEV) build kernel nested
	$(CELLA_DEV) build rootfs nested
	$(CELLA_DEV) build rootfs inception

## Resolve kernel-fragment.config against defconfig and report any line
## kconfig silently overruled (seconds, no compile)
kernel-config-check:
	$(LOG)
	$(SCRIPTS)/build/kernel-config-check.sh

# --- Everything -----------------------------------------------------

## make test, plus every KVM smoke test (skips gracefully without KVM)
test-all: test golden smoke
	$(LOG)
	echo ""
	echo "=== make test-all: done (see above for any SKIPs) ==="

## Report source-only and source+test line counts (see also README's line-
## count section)
lines:
	$(LOG)
	python3 $(SCRIPTS)/utils/count_lines.py

## Delete the run logs in .logs/, and keep the newest one for each target
logs-clean:
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

## cargo clean
clean:
	$(LOG)
	$(CARGO) clean

## clean + remove the built goldens' caches
distclean: clean
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

## KVM_SET_SREGS ordering: does CS.L=1 need CR0.PG/EFER.LMA set in the *same*
## ioctl call? (no /dev/kvm needed beyond opening it; boots nothing -- see
## src/bin/cella-probe/sregs.rs)
probe-sregs: build
	$(LOG)
	target/smoke/cella-probe sregs

## Does the guest's wall-clock land near real time at boot, with no RTC
## device? (needs /dev/kvm; see src/bin/cella-probe/wallclock.rs)
probe-wallclock: build-smoke golden
	$(LOG)
	target/smoke/cella-probe wallclock

## Does freeze/thaw leak real elapsed time into the guest's clock? (needs
## /dev/kvm + tap0, takes ~15s; see src/bin/cella-probe/freeze_thaw_clock.rs)
probe-freeze-thaw-clock: build-smoke golden
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
## probe-freeze-thaw-clock with the stage-2 prefault at thaw
## (CELLA_THAW_PREFAULT=ept)
probe-prefault-ept: build-smoke golden
	$(LOG)
	CELLA_THAW_PREFAULT=ept target/smoke/cella-probe freeze-thaw-clock

## The freeze and thaw clock probe one layer deep: cella freezes and thaws a
## guest inside a cella guest
probe-inception: build-smoke golden-nested
	$(LOG)
	$(SCRIPTS)/test/inception.sh

## Watch the thawed guest for 30 s: any kernel complaint (watchdog, unstable,
## oops) is a FAIL
probe-thaw-gate: build-smoke golden
	$(LOG)
	CELLA_POST_THAW_SECS=30 target/smoke/cella-probe freeze-thaw-clock
