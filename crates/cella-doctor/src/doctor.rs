//! cella doctor: the host judged, one fact per line.
//!
//! check reports host facts and exits nonzero on any FAIL. fix
//! repairs what the current uid can, and prints the exact command
//! for the rest. verify recomputes golden digests against their
//! manifests -- build makes, doctor judges. Facts that need root to
//! inspect degrade to a note instead of a guess.
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

/// The gate for the test scripts: quiet, one SKIP line on the
/// first unmet need, exit through the caller. Needs: kvm, bwrap,
/// golden:<axis>:<flavor>. The scripts stop re-implementing
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
                if !Path::new(&cella_libs::jail::find_program("bwrap")).is_file() {
                    println!("SKIP: bwrap not found -- run: make install");
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

    // This process's own jail. The marker is set by the parent on
    // the bwrap exec alone (cella_libs::jail::confine_self), thus its
    // presence says this incarnation came through the jail. There
    // is no escape hatch; an unconfined doctor is a FAIL of the
    // fact, stated, never a variant.
    if std::env::var(cella_libs::jail::JAILED_ENV).is_ok() {
        r.ok(
            "jail",
            "confined -- this doctor runs inside its bwrap profile",
        );
    } else {
        r.fail("jail", "unconfined -- the jail did not run");
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

    // bwrap: the jail of every VMM, and of this process. Presence by
    // path, not by running it: a bwrap inside a bwrap cannot make its
    // namespaces, and the fact doctor states is that the binary is
    // installed where the spawn looks for it.
    if Path::new(&cella_libs::jail::find_program("bwrap")).is_file() {
        r.ok("bwrap", "present");
    } else {
        r.fail("bwrap", "not found -- run: make install");
    }

    // The identity slice (1.6.14a): each machine runs as its own
    // sub-user, mapped by the spawn. That needs a delegated sub-id
    // range for this user, the setuid mapping helpers, and setfacl
    // (the spawn grants the sub-user its machine directory by ACL).
    let user = std::env::var("USER").unwrap_or_default();
    for file in ["/etc/subuid", "/etc/subgid"] {
        let delegated = fs::read_to_string(file)
            .map(|t| {
                t.lines()
                    .any(|l| l.split(':').next() == Some(user.as_str()))
            })
            .unwrap_or(false);
        if delegated {
            r.ok(file, "a sub-id range is delegated to this user");
        } else {
            r.fail(
                file,
                &format!(
                    "no delegated range -- run: sudo usermod --add-subuids {r} \
                     --add-subgids {r} $USER (make install does this)",
                    r = cella_libs::config::SUBID_RANGE_HINT
                ),
            );
        }
    }
    for tool in ["newuidmap", "newgidmap", "setfacl"] {
        let present = ["/usr/bin", "/bin", "/usr/sbin"]
            .iter()
            .any(|d| Path::new(&format!("{d}/{tool}")).is_file());
        if present {
            r.ok(tool, "present");
        } else {
            r.fail(tool, "not found -- run: make install (shadow-utils, acl)");
        }
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

    // The network (1.6.14e): no host state exists to check. Each
    // machine's translator is spawned by its start and dies at
    // its destroy; there is no pool, no unit, and no capability.

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

    if r.failed == 0 {
        println!("cella doctor: all facts hold");
    } else {
        println!("cella doctor: {} fact(s) FAIL", r.failed);
    }
    r.failed
}

/// The repairs, gathered, never run. Doctor escalates nothing and
/// builds nothing: build makes, doctor judges. fix runs check,
/// collects the exact command for every FAIL it knows how to name,
/// and prints them once, as one script the operator can read and
/// run. The one root moment stays make install's; a sub-id
/// delegation that install did not lay is the same usermod, printed.
pub fn fix() -> u32 {
    let failed = check();
    if failed == 0 {
        return 0;
    }
    let mut script: Vec<String> = Vec::new();

    // The identity slice: a missing sub-id delegation is one usermod.
    let user = std::env::var("USER").unwrap_or_default();
    let missing = ["/etc/subuid", "/etc/subgid"].iter().any(|f| {
        !fs::read_to_string(f)
            .map(|t| {
                t.lines()
                    .any(|l| l.split(':').next() == Some(user.as_str()))
            })
            .unwrap_or(false)
    });
    if missing && !user.is_empty() {
        script.push(format!(
            "sudo usermod --add-subuids {r} --add-subgids {r} {user}",
            r = cella_libs::config::SUBID_RANGE_HINT
        ));
    }

    // The goldens: an absent or unmanifested artifact rebuilds fresh,
    // so that the manifest is born with the artifact it states. The
    // kernel compile takes minutes; the build says so while it runs.
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
        script.push(format!("cella build {axis} {flavor} --fresh"));
    }

    println!();
    if script.is_empty() {
        println!(
            "cella doctor: fix -- {failed} FAIL, none with a command doctor can name; see the lines above"
        );
        return failed;
    }
    println!("cella doctor: fix -- nothing runs here. The repairs, as one script:");
    println!();
    println!("#!/usr/bin/env bash");
    println!("set -euo pipefail");
    for line in &script {
        println!("{line}");
    }
    println!();
    println!("cella doctor: then run: cella doctor check");
    failed
}

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
    failed += verify_books(name, &dir);
    failed
}

/// Walk both of a machine's books once each, checking the
/// tamper-evident chain (proto/cella.proto, Audit.predecessor and
/// Event.predecessor -- 1.6.14d). A book that has never been
/// written is a note, not a failure: a fresh machine has parked
/// nothing and no verb has audited it yet.
fn verify_books(name: &str, dir: &Path) -> u32 {
    let mut failed = 0u32;
    for (book, path) in [
        ("ledger", dir.join("network").join("ledger")),
        ("audit", dir.join("audit")),
    ] {
        let verify = if book == "ledger" {
            cella_libs::ledger::verify_ledger_chain
        } else {
            cella_libs::ledger::verify_audit_chain
        };
        if !path.is_file() {
            println!("  note  {name} {book}: no book yet -- nothing to verify");
            continue;
        }
        match verify(&path) {
            Ok(None) => {
                println!("  ok    {name} {book}: chain verifies end to end");
            }
            Ok(Some(brk)) => {
                println!(
                    "  FAIL  {name} {book}: chain breaks at entry {} -- \
                     its predecessor does not match the entry before it",
                    brk.position
                );
                failed += 1;
            }
            Err(e) => {
                println!("  FAIL  {name} {book}: reading the chain: {e}");
                failed += 1;
            }
        }
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
