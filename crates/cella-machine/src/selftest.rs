//! selftest: the installed host's acceptance gate, judged by files
//! and exit codes alone (1.6.5a). It opens with the doctor's check
//! -- a lifecycle failure must never be a disguised environment
//! failure -- thus it lives with the machine persona, over the
//! doctor's library (1.6.13).

use std::fs;

use cella_libs::machine::*;

pub fn selftest() -> Result<(), String> {
    // Doctor first: a lifecycle failure must never be a disguised
    // environment failure. The facts print either way; only the
    // /dev/kvm and bwrap facts gate the run (the rest of the checks
    // may FAIL on a host that can still run an airgapped machine).
    println!("== step 0: doctor check ==");
    cella_doctor::doctor::check();
    println!();
    let kvm_ok = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok();
    if !kvm_ok {
        println!("SKIP: no read and write access to /dev/kvm");
        return Ok(());
    }
    if std::process::Command::new("bwrap")
        .arg("--version")
        .output()
        .is_err()
    {
        println!("SKIP: bwrap not found");
        return Ok(());
    }
    // Goldens: use the real home, and seed from dist/ when possible.
    for (axis, flavor) in [("kernel", "canonical"), ("rootfs", "cella")] {
        let p = if axis == "kernel" {
            kernel_path(flavor)
        } else {
            rootfs_path(flavor)
        };
        // The build machinery lives with the build persona (1.6.13):
        // selftest judges, and points at the maker.
        if !p.is_file() {
            println!("SKIP: golden {axis} {flavor} missing -- run: cella build {axis} {flavor}");
            return Ok(());
        }
    }
    let real_kernel = kernel_path("canonical");
    let real_rootfs = rootfs_path("cella");

    // A sandboxed home, so that the test disturbs no machine. The
    // goldens link in from the real home.
    let sandbox = std::env::temp_dir().join(format!("cella-selftest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&sandbox);
    fs::create_dir_all(&sandbox).map_err(|e| format!("creating the sandbox: {e}"))?;
    std::env::set_var("CELLA_HOME", &sandbox);
    for (p, src) in [
        (kernel_path("canonical"), real_kernel),
        (rootfs_path("cella"), real_rootfs),
    ] {
        fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::copy(&src, &p).map_err(|e| format!("copying a golden: {e}"))?;
    }

    let result = selftest_cycle();
    let _ = stop("m1");
    if let Err(e) = &result {
        // A failure carries its evidence: the logs stay, and their
        // tails print.
        eprintln!("selftest failed: {e}");
        for f in ["vmm.log", "console.log"] {
            let p = machine_dir("m1").join(f);
            let content = read_lossy(&p);
            let tail = content
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .join("\n  ");
            eprintln!("-- {f} (last lines, reversed):\n  {tail}");
        }
        eprintln!("(sandbox kept: {})", sandbox.display());
        std::env::remove_var("CELLA_HOME");
        return result;
    }
    std::env::remove_var("CELLA_HOME");
    let _ = fs::remove_dir_all(&sandbox);
    println!("PASS: the lifecycle cycle, with every refusal checked");
    Ok(())
}

fn selftest_cycle() -> Result<(), String> {
    fn step(what: &str, r: Result<(), String>) -> Result<(), String> {
        r.map_err(|e| format!("{what}: {e}"))
    }
    fn refuse(what: &str, r: Result<(), String>) -> Result<(), String> {
        match r {
            Ok(()) => Err(format!("{what}: accepted, and it must refuse")),
            Err(_) => Ok(()),
        }
    }
    // Console-free by design: the installed host's acceptance gate
    // judges by files and exit codes alone, thus it runs identically
    // against the field flavor (whose machines are dark) and doubles
    // as the proof that the release binary's mouth is shut.
    let mut m = defaults();
    m.name = "m1".into();
    m.net = "none".into();
    step("create", create(&m))?;
    if valve_record("m1") != "closed" {
        return Err("not born closed: the valve record is not \"closed\"".to_string());
    }
    step("start", start("m1"))?;
    // The readiness handshake already gated start; hold a moment and
    // require the VMM to still stand.
    std::thread::sleep(std::time::Duration::from_secs(3));
    if !is_running("m1") {
        return Err("the VMM exited within 3 s of a started machine".to_string());
    }
    if cfg!(not(debug_assertions)) && machine_dir("m1").join("console.log").exists() {
        return Err("a release machine wrote a console.log -- the mouth must be shut".to_string());
    }
    refuse("double start", start("m1"))?;
    step("freeze", freeze("m1"))?;
    if !is_frozen("m1") {
        return Err("no sidecar after the freeze".to_string());
    }
    if machine_dir("m1").join("state.tmp").exists() {
        return Err("state.tmp left behind -- the rename step did not happen".to_string());
    }
    refuse("start while frozen", start("m1"))?;
    refuse("stop while frozen", stop("m1"))?;
    step("thaw", thaw("m1"))?;
    if is_frozen("m1") {
        return Err("the sidecar survived the thaw".to_string());
    }
    if !is_running("m1") {
        return Err("the VMM did not stand after the thaw".to_string());
    }
    refuse("thaw while running", thaw("m1"))?;
    step("stop", stop("m1"))?;
    step("restart", start("m1"))?;
    step("stop again", stop("m1"))?;
    step("destroy", destroy("m1"))?;
    // The installed world's negative, when the pool offers a free
    // tap: a machine is born closed, a ping gets nothing, and no
    // freeze happens on inbound traffic.
    if std::path::Path::new("/sys/class/net/tap0").exists()
        && !claimed_taps().contains(&"tap0".to_string())
    {
        let mut n = defaults();
        n.name = "m2".into();
        n.net = "tap0".into();
        step("create (closed, tap)", create(&n))?;
        step("start (closed, tap)", start("m2"))?;
        std::thread::sleep(std::time::Duration::from_secs(2));
        let answered = std::process::Command::new("ping")
            .args(["-c", "1", "-W", "2", "192.168.200.2"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if answered {
            let _ = stop("m2");
            let _ = destroy("m2");
            return Err("a closed machine answered a ping".to_string());
        }
        if is_frozen("m2") {
            let _ = stop("m2");
            let _ = destroy("m2");
            return Err("a closed machine froze on inbound traffic".to_string());
        }
        step("stop (closed, tap)", stop("m2"))?;
        step("destroy (closed, tap)", destroy("m2"))?;
        println!("  the closed machine answered nothing, and never froze");
    } else {
        println!("  (no free tap0 -- the born-closed negative did not run)");
    }
    Ok(())
}
