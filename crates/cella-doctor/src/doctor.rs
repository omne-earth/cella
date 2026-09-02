//! cella doctor: the host judged, one fact per line.
//!
//! check reports host facts and exits nonzero on any FAIL. fix
//! repairs what the current uid can, and prints the exact command
//! for the rest. verify recomputes golden digests against their
//! manifests -- build makes, doctor judges. Facts that need root to
//! inspect (nft tables) degrade to a note instead of a guess.
//! Becomes its own thin CLI at the split (see tasks/PHASE1.md).

use std::fs;
use std::path::Path;

use cella_libs::{golden, machine};

struct Report {
    failed: u32,
}

impl Report {
    fn ok(&mut self, what: &str, detail: &str) {
        println!("  ok    {what}: {detail}");
    }
    fn fail(&mut self, what: &str, detail: &str) {
        println!("  FAIL  {what}: {detail}");
        self.failed += 1;
    }
    fn note(&mut self, what: &str, detail: &str) {
        println!("  note  {what}: {detail}");
    }
}

fn run_out(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// The gate for the test scripts: quiet, one SKIP line on the
/// first unmet need, exit through the caller. Needs: kvm, bwrap,
/// tap, golden:<axis>:<flavor>. The scripts stop re-implementing
/// the checks that doctor owns; a script with its own asset
/// overrides (CELLA_TEST_*) keeps those checks local.
pub fn gate(needs: &[String]) -> u32 {
    for need in needs {
        match need.as_str() {
            "kvm" => {
                let rw =
                    unsafe { libc::access(c"/dev/kvm".as_ptr(), libc::R_OK | libc::W_OK) } == 0;
                if !rw {
                    println!("SKIP: no read and write access to /dev/kvm");
                    return 3;
                }
            }
            "bwrap" => {
                if run_out("bwrap", &["--version"]).is_none() {
                    println!("SKIP: bwrap not found -- run: make install-release");
                    return 3;
                }
            }
            "tap" => {
                let ok = run_out("ip", &["-br", "addr", "show", "tap0"])
                    .map(|s| s.contains("192.168.200.1"))
                    .unwrap_or(false);
                if !ok {
                    println!("SKIP: tap0 is not configured -- run: cella doctor fix");
                    return 3;
                }
            }
            g => {
                let Some(rest) = g.strip_prefix("golden:") else {
                    println!("SKIP: unknown gate need {need:?}");
                    return 3;
                };
                let Some((axis, flavor)) = rest.split_once(':') else {
                    println!("SKIP: unknown gate need {need:?}");
                    return 3;
                };
                let p = if axis == "kernel" {
                    machine::kernel_path(flavor)
                } else {
                    machine::rootfs_path(flavor)
                };
                if !p.is_file() {
                    println!("SKIP: golden {axis} {flavor} missing -- run: make golden");
                    return 3;
                }
            }
        }
    }
    0
}

/// The host facts. Returns the number of FAIL lines.
pub fn check() -> u32 {
    let mut r = Report { failed: 0 };
    println!("cella doctor: host facts");

    // The flavor of this binary. The field flavor (release) has no
    // console; the lab flavor (debug-assertions on) keeps it as the
    // instrument, under the -debug names.
    if cfg!(debug_assertions) {
        r.ok("flavor", "debug -- the console exists (the lab)");
    } else {
        r.ok("flavor", "release -- no console exists (the field)");
    }

    // /dev/kvm: the one device that makes a machine possible.
    let kvm = Path::new("/dev/kvm");
    if !kvm.exists() {
        r.fail(
            "/dev/kvm",
            "absent -- enable virtualization, load kvm_intel/kvm_amd",
        );
    } else {
        let rw = unsafe { libc::access(c"/dev/kvm".as_ptr(), libc::R_OK | libc::W_OK) } == 0;
        if rw {
            r.ok("/dev/kvm", "read-write");
        } else {
            r.fail("/dev/kvm", "present but not read-write for this user");
        }
    }

    // bwrap: the jail of every VMM.
    if run_out("bwrap", &["--version"]).is_some() {
        r.ok("bwrap", "present");
    } else {
        r.fail("bwrap", "not found -- run: make install-release");
    }

    // Nested KVM: required by the nested and inception images only.
    let nested = ["kvm_intel", "kvm_amd"].iter().any(|m| {
        fs::read_to_string(format!("/sys/module/{m}/parameters/nested"))
            .map(|v| {
                let v = v.trim();
                v == "Y" || v == "1"
            })
            .unwrap_or(false)
    });
    if nested {
        r.ok("nested kvm", "enabled");
    } else {
        r.note(
            "nested kvm",
            "off -- nested/inception images need it, the rest do not",
        );
    }

    // The tap pool: existence, address, and the deterministic MAC.
    let mut taps = 0u32;
    if let Ok(entries) = fs::read_dir("/sys/class/net") {
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.strip_prefix("tap")
                    .and_then(|s| s.parse::<u32>().ok())
                    .map(|_| n)
            })
            .collect();
        names.sort();
        for tap in names {
            taps += 1;
            let n: u32 = tap.strip_prefix("tap").unwrap().parse().unwrap();
            let mac = fs::read_to_string(format!("/sys/class/net/{tap}/address"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let want_mac = format!("02:ce:11:a0:00:{n:02x}");
            let host_ip = format!("192.168.{}.1", 200 + n);
            let addressed = run_out("ip", &["-br", "addr", "show", &tap])
                .map(|s| s.contains(&host_ip))
                .unwrap_or(false);
            match (mac == want_mac, addressed) {
                (true, true) => r.ok(&tap, &format!("{host_ip}/24, mac {mac}")),
                (_, false) => r.fail(&tap, &format!("missing {host_ip} -- run: cella doctor fix")),
                (false, true) => r.note(&tap, &format!("mac {mac}, not the convention {want_mac} (predates it, or hand-made); a recreation will differ")),
            }
        }
    }
    if taps == 0 {
        r.fail("tap pool", "no taps -- run: cella doctor fix (a reboot clears the pool; make install-release enables cella-network.service to recreate it at boot)");
    }

    // The boot unit: without it a reboot silently eats the pool.
    match run_out("systemctl", &["is-enabled", "cella-network.service"]) {
        Some(v) if v.trim() == "enabled" => r.ok("boot unit", "cella-network.service enabled"),
        _ => r.fail(
            "boot unit",
            "cella-network.service not enabled -- run: make install-release (a reboot then keeps the pool)",
        ),
    }

    // Forwarding: guest egress dies without it.
    match fs::read_to_string("/proc/sys/net/ipv4/ip_forward") {
        Ok(v) if v.trim() == "1" => r.ok("ip_forward", "on"),
        _ => r.fail("ip_forward", "off -- run: cella doctor fix"),
    }

    // The nft tables need root to inspect: state the fact, no guess.
    if unsafe { libc::geteuid() } == 0 {
        match run_out("nft", &["list", "table", "inet", "cella_nat"]) {
            Some(_) => r.ok("nat", "table inet cella_nat present"),
            None => r.fail(
                "nat",
                "table inet cella_nat absent -- run: cella doctor fix",
            ),
        }
    } else {
        r.note(
            "nat",
            "needs root to inspect -- run: sudo cella doctor check",
        );
    }

    // The goldens, and their manifests.
    for (axis, flavor) in [
        ("kernel", "canonical"),
        ("rootfs", "canonical"),
        ("rootfs", "cella"),
        ("rootfs", "gateway"),
    ] {
        let p = if axis == "kernel" {
            machine::kernel_path(flavor)
        } else {
            machine::rootfs_path(flavor)
        };
        let label = format!("{axis} {flavor}");
        if !p.is_file() {
            r.fail(
                &label,
                &format!("absent -- run: cella build {axis} {flavor}"),
            );
        } else if !golden::manifest_path(&p).is_file() {
            r.fail(
                &label,
                &format!("no manifest -- run: cella build {axis} {flavor}"),
            );
        } else {
            r.ok(&label, "present, with manifest");
        }
    }

    // The build toolbox: only the build verb needs it.
    match run_out("podman", &["container", "exists", "cella-build"]) {
        Some(_) => r.ok("toolbox", "cella-build present"),
        None => r.note(
            "toolbox",
            "cella-build absent -- the build verb creates it on first use",
        ),
    }

    if r.failed == 0 {
        println!("cella doctor: all facts hold");
    } else {
        println!("cella doctor: {} fact(s) FAIL", r.failed);
    }
    r.failed
}

/// Repair what fix can, then re-check. Net facts (the tap pool,
/// ip_forward, the NAT) go to cella-network, which carries
/// CAP_NET_ADMIN as a file capability -- no sudo in the path, the
/// root moment happened at install time. Everything else prints its
/// command; doctor escalates nothing.
pub fn fix() -> u32 {
    let failed = check();
    if failed == 0 {
        return 0;
    }
    println!();
    let net_bin = sibling("cella-network");
    println!("cella doctor: fix -- running {net_bin} setup");
    let ran = std::process::Command::new(&net_bin)
        .args(["setup"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ran {
        println!(
            "cella doctor: {net_bin} failed -- run: make install-release (grants it cap_net_admin)"
        );
    }
    // The goldens: building needs no root, thus fix builds what is
    // absent or unmanifested. The kernel compile takes minutes; the
    // build says what it does while it does it.
    for (axis, flavor) in [
        ("kernel", "canonical"),
        ("rootfs", "canonical"),
        ("rootfs", "cella"),
        ("rootfs", "gateway"),
    ] {
        let p = if axis == "kernel" {
            machine::kernel_path(flavor)
        } else {
            machine::rootfs_path(flavor)
        };
        if p.is_file() && golden::manifest_path(&p).is_file() {
            continue;
        }
        println!("cella doctor: fix -- building {axis} {flavor} (minutes for the kernel)");
        // Green-field: an artifact without a manifest rebuilds fresh,
        // so that the manifest is born with the artifact it states.
        if let Err(e) = cella_build::flags::build_flags(axis, flavor, true) {
            println!("cella doctor: build {axis} {flavor} failed: {e}");
        }
    }
    println!();
    println!("cella doctor: re-check");
    check()
}

/// A sibling thin CLI: beside the current binary when present (an
/// installation, or target/release), else by PATH.
fn sibling(name: &str) -> String {
    if let Ok(me) = std::env::current_exe() {
        let p = me.parent().unwrap().join(name);
        if p.is_file() {
            return p.to_string_lossy().to_string();
        }
    }
    name.to_string()
}

/// Recompute a machine's layer digests against its manifest, where
/// a universe operation recorded them. Absent digests are a note:
/// only branch and archive write them.
pub fn verify_machine(name: &str) -> u32 {
    let dir = machine::machine_dir(name);
    let Ok(raw) = fs::read_to_string(dir.join("manifest.json")) else {
        println!("  FAIL  {name}: no manifest");
        return 1;
    };
    let mut failed = 0u32;
    let mut seen = 0u32;
    for (key, layer) in [("digest_disk", "disk.img"), ("digest_ram", "ram.img")] {
        let Some(recorded) = cella_libs::machine::manifest_field(&raw, key) else {
            continue;
        };
        seen += 1;
        match golden::sha3_256_hex(&dir.join(layer)) {
            Ok(actual) if actual == recorded => {
                println!("  ok    {name} {layer}: sha3-256 {}", &actual[..16]);
            }
            Ok(actual) => {
                println!(
                    "  FAIL  {name} {layer}: digest mismatch (manifest {}.., layer {}..)",
                    &recorded[..16],
                    &actual[..16]
                );
                failed += 1;
            }
            Err(e) => {
                println!("  FAIL  {name} {layer}: {e}");
                failed += 1;
            }
        }
    }
    if seen == 0 {
        println!("  note  {name}: no recorded digests (branch and archive write them)");
    }
    failed
}

/// Recompute the digest of each golden against its manifest. A
/// target narrows it: `verify kernel canonical`, `verify <vm>`.
pub fn verify(target: Option<(&str, &str)>) -> u32 {
    let all = [
        ("kernel", "canonical"),
        ("kernel", "nested"),
        ("rootfs", "canonical"),
        ("rootfs", "cella"),
        ("rootfs", "gateway"),
        ("rootfs", "nested"),
        ("rootfs", "inception"),
    ];
    let mut failed = 0u32;
    let mut seen = 0u32;
    println!("cella doctor: verify");
    for (axis, flavor) in all {
        if let Some((a, f)) = target {
            if a != axis || f != flavor {
                continue;
            }
        }
        let p = if axis == "kernel" {
            machine::kernel_path(flavor)
        } else {
            machine::rootfs_path(flavor)
        };
        if !p.is_file() {
            if target.is_some() {
                println!("  FAIL  {axis} {flavor}: artifact absent");
                failed += 1;
            }
            continue;
        }
        seen += 1;
        let mpath = golden::manifest_path(&p);
        let Ok(text) = fs::read_to_string(&mpath) else {
            println!("  FAIL  {axis} {flavor}: no manifest -- run: cella build {axis} {flavor}");
            failed += 1;
            continue;
        };
        let Some(recorded) = golden::field(&text, "sha3_256") else {
            println!(
                "  FAIL  {axis} {flavor}: manifest carries no sha3_256 -- rebuild deliberately: cella build {axis} {flavor} --fresh"
            );
            failed += 1;
            continue;
        };
        match golden::sha3_256_hex(&p) {
            Ok(actual) if actual == recorded => {
                println!("  ok    {axis} {flavor}: sha3-256 {}", &actual[..16]);
            }
            Ok(actual) => {
                println!(
                    "  FAIL  {axis} {flavor}: digest mismatch (manifest {}.., artifact {}..) \
                     -- doctor deletes nothing; rebuild deliberately: cella build {axis} {flavor} --fresh",
                    &recorded[..16],
                    &actual[..16]
                );
                failed += 1;
            }
            Err(e) => {
                println!("  FAIL  {axis} {flavor}: {e}");
                failed += 1;
            }
        }
    }
    if seen == 0 && failed == 0 {
        println!("  note  nothing to verify -- no goldens found");
    }
    if failed == 0 {
        println!("cella doctor: verified");
    } else {
        println!("cella doctor: {failed} FAIL");
    }
    failed
}

/// The AVC harvest: denials are correlated, never captured. The
/// audit book's host clocks are the ausearch join key -- this verb
/// reads the book's time window, asks ausearch for the matching AVC
/// denials, and files the result beside the book as `avc`. It is
/// privileged and optional: the debugger exists before the lane
/// that generates the denials (the shakedown's SELinux work). A
/// permissive or clean host files an empty set and says so.
pub fn harvest(vm: Option<&str>) -> u32 {
    let book = match vm {
        Some(vm) => cella_libs::machine::machine_dir(vm).join("audit"),
        None => cella_libs::machine::home().join("audit"),
    };
    if !book.is_file() {
        println!(
            "cella doctor: no audit book at {} -- nothing to correlate",
            book.display()
        );
        return 1;
    }
    let messages = match cella_libs::ledger::read_all(&book) {
        Ok(m) => m,
        Err(e) => {
            println!("cella doctor: reading the audit book: {e}");
            return 1;
        }
    };
    let clocks: Vec<u64> = messages
        .iter()
        .filter_map(|m| match &m.body {
            Some(cella_libs::proto::message::Body::Audit(a)) => Some(a.host_ns),
            _ => None,
        })
        .collect();
    let (Some(&first), Some(&last)) = (clocks.iter().min(), clocks.iter().max()) else {
        println!("cella doctor: the audit book holds no entries -- nothing to correlate");
        return 1;
    };
    let fmt = |ns: u64, round_up: bool| -> Option<String> {
        let secs = ns / 1_000_000_000 + if round_up { 1 } else { 0 };
        let out = std::process::Command::new("date")
            .args(["-d", &format!("@{secs}"), "+%m/%d/%Y %H:%M:%S"])
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let (Some(start), Some(end)) = (fmt(first, false), fmt(last, true)) else {
        println!("cella doctor: date failed -- cannot shape the window");
        return 1;
    };
    let out = match std::process::Command::new("ausearch")
        .args(["-m", "avc", "-ts"])
        .args(start.split(' '))
        .arg("-te")
        .args(end.split(' '))
        .output()
    {
        Ok(o) => o,
        Err(_) => {
            println!("cella doctor: ausearch not found -- install audit, or run where it exists");
            return 1;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() && !err.contains("no matches") && !text.trim().is_empty() {
        println!("cella doctor: ausearch refused ({}) -- the harvest is privileged: sudo cella doctor harvest", err.trim());
        return 1;
    }
    let avc = book.with_file_name("avc");
    let denials = text.lines().filter(|l| l.contains("avc:")).count();
    let content = if text.trim().is_empty() || err.contains("no matches") {
        format!("no matching denials in the window {start} .. {end} (permissive, or clean)\n")
    } else {
        text.to_string()
    };
    if let Err(e) = std::fs::write(&avc, content) {
        println!("cella doctor: writing {}: {e}", avc.display());
        return 1;
    }
    println!(
        "cella doctor: harvested {denials} denial(s) into {} (window {start} .. {end})",
        avc.display()
    );
    0
}
