//! cella-gateway: the border's verbs -- show, release, refuse,
//! inspect, open, close. Unprivileged: files and signals on the
//! machine's own directory, never a capability, never an exec.

mod gateway;

fn usage_error(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

fn fatal(msg: &str) -> ! {
    eprintln!("cella: fatal: {msg}");
    std::process::exit(1);
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: cella gateway <vm> <show [incoming|outgoing] [--all] | release <id> | refuse <id> [--why TEXT] | inspect <id> | open | close>";
    let (Some(vm), Some(verb)) = (argv.first(), argv.get(1)) else {
        usage_error(usage)
    };
    // The witnessed border (1.6.11): one door for this persona.
    if let Err(e) = cella_libs::audit::witness(Some(vm), "gateway", &argv) {
        fatal(&e);
    }
    let rest = &argv[2..];
    let ok = match verb.as_str() {
        "show" => {
            let all = rest.iter().any(|s| s == "--all");
            let dir = rest
                .iter()
                .find(|s| *s == "incoming" || *s == "outgoing")
                .map(|s| s.as_str());
            gateway::show(vm, all, dir)
        }
        "release" => match rest {
            [id] => gateway::release(vm, id),
            _ => usage_error(usage),
        },
        "refuse" => match rest {
            [id] => gateway::refuse(vm, id, "refused"),
            [id, flag, why] if flag == "--why" => gateway::refuse(vm, id, why),
            _ => usage_error(usage),
        },
        "inspect" => match rest {
            [id] => gateway::inspect(vm, id),
            _ => usage_error(usage),
        },
        "close" => gateway::close(vm),
        "open" => gateway::open(vm),
        _ => usage_error(usage),
    };
    match ok {
        Ok(()) => std::process::exit(0),
        Err(e) => fatal(&e),
    }
}
