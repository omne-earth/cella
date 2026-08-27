SHELL := /usr/bin/env bash

CARGO ?= cargo
SCRIPTS := scripts
DIST := dist

# Local overrides, not committed -- copy .env.example to .env to change these.
-include .env
CELLA_TAP ?= tap0
CELLA_TAP_CIDR ?= 192.168.200.1/24

.PHONY: help build debug check lint fmt fmt-check \
        unit-test integration-test selftest test test-all \
        init dist setup-tap \
        smoke smoke-boot smoke-thaw smoke-net smoke-clean test-jail test-seccomp \
        clean distclean lines

help: ## Show this help
	@echo "cella -- build, lint, and test targets"
	@echo ""
	@echo "Build:"
	@grep -hE '^(build|debug|check|lint|fmt|fmt-check):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t'
	@echo ""
	@echo "Tests that need no /dev/kvm (unit + integration, run anywhere):"
	@grep -hE '^(unit-test|integration-test|selftest|test|test-jail|test-seccomp):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t'
	@echo ""
	@echo "Smoke tests: real KVM, a real guest (one target per workflow):"
	@grep -hE '^(smoke|smoke-boot|smoke-thaw|smoke-net|smoke-clean):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t'
	@echo ""
	@echo "Setup:"
	@grep -hE '^(init|dist|setup-tap):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t'
	@echo ""
	@echo "Everything:"
	@grep -hE '^(test-all|clean|distclean|lines):.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | column -t -s $$'\t'

# --- Build ------------------------------------------------------------

build: ## Release build (target/release/cella)
	$(CARGO) build --release

debug: ## Debug build (target/debug/cella), faster to compile
	$(CARGO) build

check: ## cargo check, no codegen
	$(CARGO) check --all-targets

lint: fmt-check ## cargo clippy (all targets) + fmt-check
	$(CARGO) clippy --all-targets

fmt: ## Apply cargo fmt
	$(CARGO) fmt

fmt-check: ## Verify formatting without changing files (CI-friendly)
	$(CARGO) fmt -- --check

# --- Tests that need no /dev/kvm ---------------------------------------
#
# These are the ones that run in an ordinary container/CI runner. Split
# per kind (unit vs. integration) so `make unit-test` stays fast during
# iteration; `make test` runs everything in this section.

unit-test: ## cargo test --lib (inline #[cfg(test)] modules)
	$(CARGO) test --lib

integration-test: ## cargo test --tests (tests/*.rs, real virtio-mmio/blk logic, no KVM)
	$(CARGO) test --tests

selftest: build ## Sanity-run the seccomp self-test binary directly (see also: make test-seccomp)
	@./target/release/cella --selftest-seccomp; \
	status=$$?; \
	if [ $$status -eq 159 ]; then echo "OK: killed by SIGSYS as expected (exit $$status)"; \
	else echo "UNEXPECTED exit $$status"; exit 1; fi

test-jail: build ## Rootless bwrap jail actually confines the process (scripts/test/jail.sh)
	@$(SCRIPTS)/test/jail.sh

test-seccomp: build ## The real BPF filter kills a disallowed syscall (scripts/test/seccomp.sh)
	@$(SCRIPTS)/test/seccomp.sh

test: check lint unit-test integration-test test-jail test-seccomp ## Everything above: build hygiene + all no-KVM tests
	@echo ""
	@echo "=== make test: all no-KVM checks passed ==="

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
	@$(SCRIPTS)/test/boot.sh

smoke-thaw: build dist ## Boot -> freeze (SIGUSR1) -> verify sidecar -> thaw -> one-shot check (scripts/test/thaw.sh)
	@$(SCRIPTS)/test/thaw.sh

smoke-net: build dist ## Guest answers ICMP over the TAP after boot (scripts/test/net.sh, best-effort)
	@$(SCRIPTS)/test/net.sh

smoke: smoke-boot smoke-thaw smoke-net ## All three smoke-* targets (skips gracefully without KVM)
	@echo ""
	@echo "=== make smoke: done (see above for any SKIPs) ==="

smoke-clean: ## Kill any stray cella process left running by an interrupted smoke test
	@pkill -f 'target/(release|debug)/cella' && echo "cella: killed stray process(es)" || echo "cella: nothing to clean up"

# --- Setup --------------------------------------------------------------

.toolbox: ## Sentinel: creates + provisions the cella-build toolbox (kernel build toolchain lives there, not on the host)
	$(SCRIPTS)/build/toolbox.sh
	@touch .toolbox

init: ## One-time host setup (Fedora): installs runtime deps, provisions the build toolbox, creates tap0, builds dist, checks /dev/kvm (needs sudo)
	@$(SCRIPTS)/setup/bootstrap.sh
	@$(MAKE) .toolbox
	@$(MAKE) setup-tap
	@$(MAKE) dist

$(DIST)/bzImage $(DIST)/rootfs.ext4: | .toolbox
	@$(SCRIPTS)/build/assets.sh

dist: $(DIST)/bzImage $(DIST)/rootfs.ext4 ## Build a minimal rootfs + bzImage kernel from source (compiled inside the toolbox), skipped if already built

setup-tap: ## One-time (per boot) TAP device creation -- needs sudo once (name/CIDR from .env, see .env.example)
	sudo $(SCRIPTS)/setup/tap.sh $(CELLA_TAP) $(CELLA_TAP_CIDR)

# --- Everything -----------------------------------------------------

test-all: test dist smoke ## make test, plus every KVM smoke test (skips gracefully without KVM)
	@echo ""
	@echo "=== make test-all: done (see above for any SKIPs) ==="

lines: ## Report source-only and source+test line counts (see also README's line-count section)
	@python3 $(SCRIPTS)/utils/count_lines.py

clean: ## cargo clean
	$(CARGO) clean

distclean: clean ## clean + remove built dist/ assets
	rm -rf $(DIST)
