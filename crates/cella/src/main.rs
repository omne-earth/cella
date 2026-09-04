//! cella: the shim. It owns zero verbs; every verb belongs to a
//! persona binary, and the shim's whole job is the routing table
//! and one exec. The interface stays one word; the confinement
//! stays per binary (1.6.13). The only execs in the system are
//! boundaries by nature: this shim into the personas, and the
//! machine persona into cella-vmm.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;

/// The persona that owns a verb. The shim knows who owns what and
/// nothing else.
fn persona_for(verb: &str) -> Option<&'static str> {
    Some(match verb {
        "create" | "start" | "stop" | "enter" | "freeze" | "thaw" | "destroy" | "list" | "info"
        | "selftest" => "cella-machine",
        "gateway" => "cella-gateway",
        "branch" | "archive" | "inspect" => "cella-universe",
        "build" => "cella-build",
        "doctor" => "cella-doctor",
        "network" => "cella-network",
        "probe" => "cella-probe",
        _ => return None,
    })
}

/// A sibling binary beside this shim's own inode, same flavor: a
/// -debug shim execs -debug personas (the probe's rule,
/// generalized). The field shim never execs a lab binary.
fn sibling(name: &str) -> PathBuf {
    let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cella"));
    let dir = me.parent().map(PathBuf::from).unwrap_or_default();
    let flavored = me
        .file_name()
        .map(|n| n.to_string_lossy().ends_with("-debug"))
        .unwrap_or(false);
    if flavored {
        dir.join(format!("{name}-debug"))
    } else {
        dir.join(name)
    }
}

fn print_help() {
    println!("cella -- a cryogenic world for agents");
    println!();
    println!("The machine lifecycle (see docs/LIFECYCLE.md):");
    println!("  cella build <kernel|rootfs> <flavor> [--fresh]  build one golden artifact");
    println!(
        "  cella create <machine> [options]                   stage a machine from the goldens"
    );
    println!(
        "  cella start <machine>                              run the machine, detached and jailed"
    );
    println!("  cella enter <machine>                              attach the console (the lab flavor only)");
    println!(
        "  cella freeze <machine>                             stop the machine and keep the instant"
    );
    println!("  cella thaw <machine>                               resume the instant");
    println!("  cella stop <machine>                               end the machine and clear the transients");
    println!("  cella destroy <machine>                            delete the machine");
    println!(
        "  cella list                                      show each machine, one line per machine"
    );
    println!(
        "  cella info <machine>                               show the full record of one machine"
    );
    println!("  cella gateway <machine> <verb>                     operate the border: show, release, refuse, inspect, open, close");
    println!(
        "  cella branch <machine> <new> | archive <machine> | inspect <machine>  operate on machines as artifacts"
    );
    println!(
        "  cella doctor check|fix|verify|harvest           examine, repair, and audit the host"
    );
    println!("  cella selftest                                  run the full lifecycle against a real guest");
    println!();
    println!("Each verb runs in its own persona binary; the shim only routes.");
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(first) = argv.first() else {
        print_help();
        std::process::exit(0);
    };
    if matches!(first.as_str(), "help" | "--help" | "-h") {
        print_help();
        std::process::exit(0);
    }
    // The flag interface (--state-dir, --dump-ledger, --dump-state,
    // --print-default-cmdline, --selftest-seccomp) is the VMM's
    // alone: leading-dash invocations route there.
    if first.starts_with("--") {
        let bin = sibling("cella-vmm");
        let err = std::process::Command::new(&bin).args(&argv).exec();
        eprintln!("cella: exec {}: {err}", bin.display());
        std::process::exit(1);
    }
    let Some(persona) = persona_for(first) else {
        eprintln!("cella: unknown verb {first:?} -- cella --help lists them");
        std::process::exit(2);
    };
    let bin = sibling(persona);
    // The verb word stays in the persona's argv where the persona
    // expects it: cella-gateway takes <machine> first (the verb is its
    // name); the machine and universe personas take the verb.
    let args: Vec<&String> = match first.as_str() {
        "gateway" | "build" | "doctor" | "probe" | "network" => argv[1..].iter().collect(),
        _ => argv.iter().collect(),
    };
    let err = std::process::Command::new(&bin).args(&args).exec();
    eprintln!("cella: exec {}: {err}", bin.display());
    std::process::exit(1);
}
