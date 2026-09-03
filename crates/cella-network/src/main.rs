//! cella-network: the translator (1.6.14e).
//!
//! One process per machine, machine-lifetime, no capability: the
//! machine's start spawns `cella-network edge <vm>`, destroy kills
//! it, and it stands across every freeze and thaw. It holds the
//! machine's edges -- wires to other translators, and the world
//! side over unprivileged sockets. Nothing about the network is
//! host state anymore; see docs/ROOTLESS-NETWORK.md.

mod seccomp;

mod edge;
mod tcp;
mod world;

fn main() {
    // Hidden self-test hook for `make test-seccomp-network`. See
    // seccomp.rs's module doc: the real verbs stay unconfined until
    // 1.6.14e rewrites this persona's surface.
    if std::env::args().nth(1).as_deref() == Some("--selftest-seccomp") {
        seccomp::selftest_provoke_kill();
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The witnessed border: setup and pair are placeless verbs, and
    // the wiring binary witnesses them like every other verb (the
    // static gate of make test counts this door).
    if let Some(verb) = args.first() {
        if let Err(e) = cella_libs::audit::witness(None, verb, &args[1..]) {
            eprintln!("cella-network: {e}");
            std::process::exit(1);
        }
    }
    let mut it = args.iter();
    match it.next().map(|s| s.as_str()) {
        Some("edge") => {
            // The translator (1.6.14e rung 2): one process per
            // machine, machine-lifetime, wires only at this rung.
            // The machine's start spawns it detached; destroy
            // kills it by edge.pid.
            let Some(vm) = args.get(1) else {
                eprintln!("usage: cella-network edge <vm>");
                std::process::exit(2);
            };
            if let Err(e) = edge::run(vm) {
                eprintln!("cella-network: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some("--help") | Some("-h") => {
            println!("cella-network -- the tap pool, without sudo");
            println!("usage: cella-network setup [--taps N] [--from N] | pair --id N --via tap<n>");
            println!("needs cap_net_admin (make install grants it) or root");
            std::process::exit(0);
        }
        Some(other) => {
            eprintln!("cella-network: unknown verb {other:?} -- usage: cella-network edge <vm>");
            std::process::exit(2);
        }
        None => {
            eprintln!("usage: cella-network edge <vm>");
            std::process::exit(2);
        }
    }
}
