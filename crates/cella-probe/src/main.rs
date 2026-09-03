//! cella-probe: the diagnostics, installable.
//!
//! One binary, one probe per subcommand. The probes verified the
//! cryogenic principle from day one as standalone crates that cargo
//! built at run time; an installed host must answer "is time
//! cryogenic here" without a toolchain, thus they live here now
//! (see tasks/PHASE1.md). Parameters stay environment variables (CELLA_*),
//! the same interface the make targets always passed.

mod freeze_thaw_clock;

mod seccomp;
mod sregs;
mod wallclock;

fn main() {
    // Hidden self-test hook for `make test-seccomp-probe`. See
    // seccomp.rs's module doc: the real probes stay unconfined.
    if std::env::args().nth(1).as_deref() == Some("--selftest-seccomp") {
        seccomp::selftest_provoke_kill();
    }
    let arg = std::env::args().nth(1).unwrap_or_default();
    match arg.as_str() {
        "wallclock" => wallclock::run(),
        "freeze-thaw-clock" => freeze_thaw_clock::run(),
        "sregs" => sregs::run(),
        _ => {
            eprintln!("cella-probe -- the cryogenic diagnostics");
            eprintln!("usage: cella-probe <wallclock|freeze-thaw-clock|sregs>");
            eprintln!("parameters: CELLA_* environment variables (see the Makefile probe section)");
            std::process::exit(2);
        }
    }
}
