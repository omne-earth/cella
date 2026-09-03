//! cella-machine: the lifecycle persona -- create, start, stop,
//! enter, freeze, thaw, destroy, list, info, selftest. It owns its
//! verbs and nothing else (1.6.13); the lifecycle core lives in
//! cella-libs (the universe's inspect is its other user).

mod seccomp;
mod selftest;

use cella_libs::machine;

const VERBS: &[&str] = &[
    "create", "start", "stop", "enter", "freeze", "thaw", "destroy", "list", "info", "selftest",
];

fn fatal(msg: &str) -> ! {
    eprintln!("cella: fatal: {msg}");
    std::process::exit(1);
}

fn usage_error(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

fn verb_machine<'a>(verb: &str, args: &'a [String]) -> Option<&'a str> {
    match verb {
        "list" | "selftest" => None,
        _ => args.first().map(|s| s.as_str()),
    }
}

fn main() {
    // A verb is a CLI citizen: `cella list | head` must not panic.
    // SAFETY: setting a signal disposition before any other work.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Hidden self-test hook for `make test-seccomp-machine`.
    if argv.first().map(String::as_str) == Some("--selftest-seccomp") {
        seccomp::selftest_provoke_kill();
    }
    let Some(verb) = argv.first().cloned() else {
        usage_error(&format!("usage: cella-machine <{}> ...", VERBS.join("|")))
    };
    let args = &argv[1..];
    if !VERBS.contains(&verb.as_str()) {
        usage_error(&format!(
            "cella-machine does not own the verb {verb:?} -- its verbs: {}",
            VERBS.join(", ")
        ));
    }
    // start/thaw/selftest fork the bwrap+cella-vmm process tree, which
    // installs its own filter downstream (see seccomp.rs's module
    // doc) -- everything else is safe to confine here.
    if seccomp::SAFE_VERBS.contains(&verb.as_str()) {
        seccomp::install().unwrap_or_else(|e| fatal(&format!("seccomp: {e}")));
    }
    // The witnessed border: every verb is an event, before it runs
    // (1.6.11; the static gate counts one door per persona).
    if let Err(e) = cella_libs::audit::witness(verb_machine(&verb, args), &verb, args) {
        fatal(&e);
    }
    let ok = match verb.as_str() {
        "create" => {
            let mut it = args.iter();
            let Some(name) = it.next() else {
                usage_error("usage: cella create <name> [--kernel F] [--rootfs F] [--mem-mb N] [--net TAP|none] [--root rw|ro]")
            };
            let mut m = machine::defaults();
            m.name = name.clone();
            let mut res = Ok(());
            while let Some(a) = it.next() {
                let mut val = |what: &str| {
                    it.next()
                        .cloned()
                        .unwrap_or_else(|| fatal(&format!("missing value for {what}")))
                };
                match a.as_str() {
                    "--kernel" => m.kernel = val("--kernel"),
                    "--rootfs" => m.rootfs = val("--rootfs"),
                    "--mem-mb" => {
                        m.mem_mb = val("--mem-mb")
                            .parse()
                            .unwrap_or_else(|_| fatal("--mem-mb must be a number"))
                    }
                    "--net" => m.net = val("--net"),
                    "--root" => m.root = val("--root"),
                    "--diag" => m.diag = "on".to_string(),
                    other => {
                        res = Err(format!("unknown create option {other:?}"));
                        break;
                    }
                }
            }
            res.and_then(|()| machine::create(&m)).map(|()| {
                let net = machine::read_manifest(&m.name)
                    .map(|r| r.net)
                    .unwrap_or_else(|_| m.net.clone());
                println!(
                    "cella: created machine {:?} at {} (net {net})",
                    m.name,
                    machine::machine_dir(&m.name).display()
                );
            })
        }
        "destroy" => match args {
            [name] => machine::destroy(name).map(|()| {
                println!("cella: destroyed machine {name:?}");
            }),
            _ => Err("usage: cella destroy <name>".to_string()),
        },
        "start" => match args {
            [name] => machine::start(name),
            _ => Err("usage: cella start <name>".to_string()),
        },
        "stop" => match args {
            [name] => machine::stop(name),
            _ => Err("usage: cella stop <name>".to_string()),
        },
        "freeze" => match args {
            [name] => machine::freeze(name),
            _ => Err("usage: cella freeze <name>".to_string()),
        },
        "thaw" => match args {
            [name] => machine::thaw(name),
            _ => Err("usage: cella thaw <name>".to_string()),
        },
        "enter" => match args {
            [name] => machine::enter(name),
            _ => Err("usage: cella enter <name>".to_string()),
        },
        "list" => match args {
            [] => machine::list(),
            _ => Err("usage: cella list".to_string()),
        },
        "info" => match args {
            [name] => machine::info(name),
            _ => Err("usage: cella info <name>".to_string()),
        },
        "selftest" => selftest::selftest(),
        _ => unreachable!(),
    };
    match ok {
        Ok(()) => std::process::exit(0),
        Err(e) => fatal(&e),
    }
}
