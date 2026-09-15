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
mod probe;
mod proxy;
mod splice;

fn usage_probe() -> ! {
    eprintln!("usage: cella-terminator --probe <name> <port> <ns[:port]> <ca.pem>");
    std::process::exit(2);
}

fn main() {
    // The build's door (phase C): `cella build rootfs terminator`
    // runs the host-built binary to mint the pair CA -- rcgen stays
    // quarantined in this crate, and the build verb links nothing.
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The gates' and the field's diagnostic voice:
    //   cella-terminator --probe <name> <port> <ns[:port]> <ca.pem>
    if args.first().map(String::as_str) == Some("--probe") {
        if let [_, name, port, ns, ca] = args.as_slice() {
            let (ns_ip, ns_port) = match ns.rsplit_once(':') {
                Some((ip, p)) => (ip.to_string(), p.parse().unwrap_or(53)),
                None => (ns.clone(), 53),
            };
            let Ok(ns) = ns_ip.parse() else {
                usage_probe();
            };
            let Ok(port) = port.parse() else {
                usage_probe();
            };
            std::process::exit(probe::run(&probe::ProbeArgs {
                name: name.clone(),
                port,
                ns,
                ns_port,
                ca_pem: std::path::PathBuf::from(ca),
            }));
        }
        usage_probe();
    }
    if let [flag, name, outdir] = args.as_slice() {
        if flag == "--mint-pair-ca" {
            match ca::mint_pair_ca(name) {
                Ok((cert_pem, key_pem)) => {
                    let out = std::path::Path::new(outdir);
                    if let Err(e) = std::fs::write(out.join("ca.pem"), cert_pem)
                        .and_then(|_| std::fs::write(out.join("ca.key"), key_pem))
                    {
                        eprintln!("cella-terminator: writing the pair CA: {e}");
                        std::process::exit(1);
                    }
                    return;
                }
                Err(e) => {
                    eprintln!("cella-terminator: minting the pair CA: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
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
