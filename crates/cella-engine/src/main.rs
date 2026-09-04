//! cella-engine: the bridge (W.B.1, docs/WORLD-ENGINE.md).
//!
//! Two verbs, both the harness's to spawn, neither the shim's:
//!
//! - `cella-engine <vm> --dial <addr>` -- the bridge: tail the
//!   machine's ledger, stream each Event to the engine, land each
//!   returned Decision in the verdict file, and kick the VMM. The
//!   bridge never decides: a halted engine means holds that wait.
//! - `cella-engine motor --listen <addr> [--allow ip:port ...]` --
//!   a stand-in engine for the gates: releases the allowed
//!   destinations, refuses the rest, logs every Event.

mod bridge;
mod motor;

/// The generated vocabulary: the same proto/cella.proto that
/// cella-libs compiles for the file wire, here with the Engine
/// service stubs. Same bytes, one contract.
pub mod pb {
    tonic::include_proto!("cella");
}

fn usage() -> ! {
    eprintln!(
        "usage: cella-engine <vm> --dial <addr> | motor --listen <addr> [--allow ip:port ...]"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(verb) = args.first() {
        if let Err(e) = cella_libs::audit::witness(None, verb, &args[1..]) {
            eprintln!("cella-engine: {e}");
            std::process::exit(1);
        }
    }
    match args.first().map(|s| s.as_str()) {
        Some("motor") => {
            if let Err(e) = motor::run(&args[1..]) {
                eprintln!("cella-engine: motor: {e}");
                std::process::exit(1);
            }
        }
        Some(vm) if !vm.starts_with('-') => {
            let dial = match args.iter().position(|a| a == "--dial") {
                Some(i) => args.get(i + 1).cloned().unwrap_or_else(|| usage()),
                None => usage(),
            };
            if let Err(e) = bridge::run(vm, &dial) {
                eprintln!("cella-engine: {e}");
                std::process::exit(1);
            }
        }
        _ => usage(),
    }
}
