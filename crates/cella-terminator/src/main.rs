//! cella-terminator: the one network appliance's userland
//! (docs/NETWORK-MODEL.md, "The terminator"; tasks/
//! PHASE2-security.md, 2.7). Guest-only: this binary ships inside
//! the terminator image like busybox does -- no witness door, no
//! install, no shim row. It is the resolver that intercepts, the
//! proxy that terminates and splices, and the minter of pair-CA
//! leafs. It runs as the image's one service under the init's
//! respawn loop.

mod ca;
mod config;
mod dns;
mod http;
mod proxy;
mod splice;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/etc/cella-terminator.conf".to_string());
    let cfg = match config::load(std::path::Path::new(&path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cella-terminator: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = proxy::run(cfg) {
        eprintln!("cella-terminator: {e}");
        std::process::exit(1);
    }
}
