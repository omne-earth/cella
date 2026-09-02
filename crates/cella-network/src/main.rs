//! cella-network: the one CAP_NET_ADMIN holder.
//!
//! The first thin CLI of the split (see tasks/PHASE1.md). install.sh grants
//! the binary cap_net_admin as a file capability, thus no invocation
//! uses sudo: the root moment happens once, at install time. The
//! binary provisions the tap pool, the addresses, the deterministic
//! MACs, ip_forward, and the NAT table -- and nothing else. The
//! firewalld zone binding stays in install.sh: it is a one-time
//! permanent name binding, and dbus/polkit wants real root.

use cella_libs::machine;

fn main() {
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
    let mut taps = 4u32;
    let mut from = 0u32;
    let mut it = args.iter();
    // The shim forwards `cella setup net ...` verbatim: tolerate the
    // "net" token after "setup".
    match it.next().map(|s| s.as_str()) {
        Some("pair") => {
            let mut id = None;
            let mut via = None;
            let mut pit = args[1..].iter();
            while let Some(a) = pit.next() {
                match a.as_str() {
                    "--id" => id = pit.next().and_then(|v| v.parse().ok()),
                    "--via" => via = pit.next().cloned(),
                    other => {
                        eprintln!("cella-network: unknown flag {other:?}");
                        std::process::exit(2);
                    }
                }
            }
            let (Some(id), Some(via)) = (id, via) else {
                eprintln!("usage: cella-network pair --id N --via tap<n>");
                std::process::exit(2);
            };
            if let Err(e) = machine::setup_pair(id, &via) {
                eprintln!("cella-network: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some("setup") | None => {}
        Some("--help") | Some("-h") => {
            println!("cella-network -- the tap pool, without sudo");
            println!("usage: cella-network setup [--taps N] [--from N] | pair --id N --via tap<n>");
            println!("needs cap_net_admin (make install-release grants it) or root");
            std::process::exit(0);
        }
        Some(other) => {
            eprintln!("cella-network: unknown verb {other:?} -- usage: cella-network setup [--taps N] [--from N]");
            std::process::exit(2);
        }
    }
    while let Some(a) = it.next() {
        if a == "net" {
            // `cella setup net ...` arrives verbatim through the shim.
            continue;
        }
        let mut val = |what: &str| -> u32 {
            it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                eprintln!("cella-network: {what} needs a number");
                std::process::exit(2);
            })
        };
        match a.as_str() {
            "--taps" => taps = val("--taps"),
            "--from" => from = val("--from"),
            other => {
                eprintln!("cella-network: unknown flag {other:?}");
                std::process::exit(2);
            }
        }
    }
    if let Err(e) = machine::setup_net(taps, from) {
        eprintln!("cella-network: {e}");
        std::process::exit(1);
    }
}
