//! cella-build: the build persona -- makes golden artifacts, and
//! nothing else. Verification belongs to doctor: build makes,
//! doctor judges.

mod seccomp_bin;

fn fatal(msg: &str) -> ! {
    eprintln!("cella-build: fatal: {msg}");
    std::process::exit(1);
}

fn main() {
    // Hidden self-test hook for `make test-seccomp-build`.
    if std::env::args().nth(1).as_deref() == Some("--selftest-seccomp") {
        seccomp_bin::selftest_provoke_kill();
    }
    seccomp_bin::install().unwrap_or_else(|e| fatal(&format!("seccomp: {e}")));
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // The witnessed border: every verb is an event (1.6.11).
    if let Err(e) = cella_libs::audit::witness(None, "build", &argv) {
        fatal(&e);
    }
    let r = match argv.as_slice() {
        [axis, flavor] => cella_build::flags::build(axis, flavor),
        [axis, flavor, flag] if flag == "--fresh" => {
            cella_build::flags::build_flags(axis, flavor, true)
        }
        _ => Err("usage: cella-build <kernel|rootfs> <flavor> [--fresh]".to_string()),
    };
    if let Err(e) = r {
        fatal(&e);
    }
}
