//! cella-probe: the diagnostics, installable.
//!
//! One binary, one probe per subcommand. The probes verified the
//! cryogenic principle from day one as standalone crates that cargo
//! built at run time; an installed host must answer "is time
//! cryogenic here" without a toolchain, thus they live here now
//! (see tasks/PHASE1.md). Parameters stay environment variables (CELLA_*),
//! the same interface the make targets always passed.

mod freeze_thaw_clock;

/// The tap follows the claim (1.6.14a/b): a machine start re-owns
/// its taps to its own sub-uid and there is no handback (a
/// seccomp-confined verb cannot hand a file capability to a child
/// -- no_new_privs strips it on exec). The probe is a claimant
/// like any other: before it spawns a VMM on a pool tap, it asks
/// the one CAP_NET_ADMIN holder to re-own the tap to the invoking
/// user. Best-effort: a tap the invoker already owns needs no call,
/// and the attach error names the real fault if this fails.
pub(crate) fn claim_tap(tap: &str) {
    let net_bin = std::env::var("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".local/bin/cella-network"))
        .ok()
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cella-network".to_string());
    // SAFETY: getuid has no failure mode.
    let me = unsafe { libc::getuid() };
    let _ = std::process::Command::new(net_bin)
        .args(["own", tap, &me.to_string()])
        .status();
}
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
