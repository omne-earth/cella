//! cella-universe: machines as artifacts -- branch, archive,
//! inspect. Running is the only state a universe verb refuses.

mod seccomp;
mod universe;

const VERBS: &[&str] = &["branch", "archive", "inspect"];

fn fatal(msg: &str) -> ! {
    eprintln!("cella: fatal: {msg}");
    std::process::exit(1);
}

fn usage_error(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

fn main() {
    // Hidden self-test hook for `make test-seccomp-universe`.
    if std::env::args().nth(1).as_deref() == Some("--selftest-seccomp") {
        seccomp::selftest_provoke_kill();
    }
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = argv.first().cloned() else {
        usage_error("usage: cella-universe <branch|archive|inspect> ...")
    };
    // inspect runs unconfined at this layer (1.6.14b): it starts its
    // throwaway appliance through machine::start, whose spawn needs
    // newuidmap -- a setuid helper that no_new_privs (mandatory for
    // an unprivileged filter) forbids from elevating. Same physics
    // as cella-machine's start/thaw; the appliance VMM's own filter
    // bounds the sensitive work, and the join's confine-after-fork
    // closes this layer too (deal-breaker 3).
    if verb != "inspect" {
        seccomp::install().unwrap_or_else(|e| fatal(&format!("seccomp: {e}")));
    }
    if !VERBS.contains(&verb.as_str()) {
        usage_error(&format!(
            "cella-universe does not own the verb {verb:?} -- its verbs: {}",
            VERBS.join(", ")
        ));
    }
    let args = &argv[1..];
    // The witnessed border (1.6.11): the source machine's book.
    if let Err(e) = cella_libs::audit::witness(args.first().map(|s| s.as_str()), &verb, args) {
        fatal(&e);
    }
    let ok = match verb.as_str() {
        "branch" => match args {
            [src, dst] => universe::branch(src, dst),
            _ => Err("usage: cella branch <existing-vm> <new-vm>".to_string()),
        },
        "archive" => match args {
            [vm] => universe::archive(vm),
            _ => Err("usage: cella archive <vm>".to_string()),
        },
        "inspect" => match args {
            [vm] => universe::inspect(vm),
            _ => Err("usage: cella inspect <vm>".to_string()),
        },
        _ => unreachable!(),
    };
    match ok {
        Ok(()) => std::process::exit(0),
        Err(e) => fatal(&e),
    }
}
