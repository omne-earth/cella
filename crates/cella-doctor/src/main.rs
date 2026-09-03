//! cella-doctor: check judges the host, fix repairs what the uid
//! can, verify audits the goldens and the machines, harvest files
//! the AVC denials beside the audit book.

mod seccomp;

fn usage_error(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Hidden self-test hook for `make test-seccomp-doctor`.
    if argv.first().map(String::as_str) == Some("--selftest-seccomp") {
        seccomp::selftest_provoke_kill();
    }
    // The witnessed border (1.6.11): a placeless verb, the root book.
    if let Err(e) = cella_libs::audit::witness(None, "doctor", &argv) {
        eprintln!("cella: fatal: {e}");
        std::process::exit(1);
    }
    // verify/verify_machine are the one pure subcommand: see
    // seccomp.rs's module doc for why check/fix/gate/harvest stay
    // unconfined at this layer.
    if argv
        .first()
        .is_some_and(|v| seccomp::SAFE_VERBS.contains(&v.as_str()))
    {
        seccomp::install().unwrap_or_else(|e| {
            eprintln!("cella: fatal: seccomp: {e}");
            std::process::exit(1);
        });
    }
    let failed = match argv.first().map(|s| s.as_str()) {
        Some("check") | None => cella_doctor::doctor::check(),
        Some("fix") => cella_doctor::doctor::fix(),
        Some("gate") => cella_doctor::doctor::gate(&argv[1..]),
        Some("harvest") => match &argv[1..] {
            [] => cella_doctor::doctor::harvest(None),
            [vm] => cella_doctor::doctor::harvest(Some(vm)),
            _ => usage_error("usage: cella doctor harvest [<vm>]"),
        },
        Some("verify") => match &argv[1..] {
            [] => cella_doctor::doctor::verify(None),
            [vm] => cella_doctor::doctor::verify_machine(vm),
            [axis, flavor] => cella_doctor::doctor::verify(Some((axis, flavor))),
            _ => usage_error("usage: cella doctor verify [<vm> | kernel|rootfs <flavor>]"),
        },
        _ => usage_error("usage: cella doctor [check|fix|verify|harvest [<vm>]|gate <needs...>]"),
    };
    std::process::exit(if failed > 0 { 1 } else { 0 });
}
