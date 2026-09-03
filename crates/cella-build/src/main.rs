//! cella-build: the build persona -- makes golden artifacts, and
//! nothing else. Verification belongs to doctor: build makes,
//! doctor judges.

mod seccomp_bin;

fn fatal(msg: &str) -> ! {
    eprintln!("cella-build: fatal: {msg}");
    std::process::exit(1);
}

fn main() {
    // Hidden self-test hook for `make test-seccomp-build`.
    if std::env::args().nth(1).as_deref() == Some("--selftest-seccomp") {
        seccomp_bin::selftest_provoke_kill();
    }
    // No production filter at this layer (found 2026-09-02, see
    // tasks/PHASE1.md #NOTES): a BPF filter is inherited across
    // exec, and this persona's whole job is exec'ing toolbox --
    // whose podman world's syscall needs are unbounded from here. A
    // filter installed above it killed toolbox on its first stat.
    // The list and the canary gate stand (the selftest hook above);
    // the join's confine-after-fork closes this layer for real
    // (deal-breakers 3-5).
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // The witnessed border: every verb is an event (1.6.11).
    if let Err(e) = cella_libs::audit::witness(None, "build", &argv) {
        fatal(&e);
    }
    let r = match argv.as_slice() {
        [axis, flavor] => cella_build::flags::build(axis, flavor),
        [axis, flavor, flag] if flag == "--fresh" => {
            cella_build::flags::build_flags(axis, flavor, true)
        }
        _ => Err("usage: cella-build <kernel|rootfs> <flavor> [--fresh]".to_string()),
    };
    if let Err(e) = r {
        fatal(&e);
    }
}
