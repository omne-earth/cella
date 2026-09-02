//! cella-universe: machines as artifacts.
//!
//! branch copies a machine, archive turns one into a rock, and
//! inspect attaches a machine's disk to a temporary appliance as
//! evidence -- read-only at the device, noexec at the mount. One
//! rule spans the family: running is the only state a universe verb
//! refuses. Every operation records the sha3-256 of the storage
//! layers it touches into the manifest it produces. See
//! docs/LIFECYCLE.md, "The universe family".

use std::fs;
use std::path::Path;

use cella_libs::{golden, machine};

/// Append flat fields to a manifest JSON string, before the closing
/// brace. The Manifest struct does not carry these fields; the raw
/// text does, and the readers use json_field.
fn with_fields(manifest_json: &str, fields: &[(String, String)]) -> String {
    let mut out = manifest_json
        .trim_end()
        .trim_end_matches('}')
        .trim_end()
        .trim_end_matches(',')
        .to_string();
    for (k, v) in fields {
        out.push_str(&format!(",\n  \"{k}\": \"{v}\""));
    }
    out.push_str("\n}\n");
    out
}

/// The digest fields of the storage layers present in a machine
/// directory: disk.img always, ram.img where present.
fn layer_digests(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let mut fields = Vec::new();
    for layer in ["disk.img", "ram.img"] {
        let p = dir.join(layer);
        if p.is_file() {
            let h = golden::sha3_256_hex(&p)?;
            let key = format!("digest_{}", layer.trim_end_matches(".img"));
            fields.push((key, h));
        }
    }
    Ok(fields)
}

fn write_manifest_text(dir: &Path, text: &str) -> Result<(), String> {
    let tmp = dir.join("manifest.tmp");
    fs::write(&tmp, text).map_err(|e| format!("write manifest: {e}"))?;
    fs::rename(&tmp, dir.join("manifest.json")).map_err(|e| format!("rename manifest: {e}"))?;
    Ok(())
}

fn refuse_running(name: &str, verb: &str) -> Result<(), String> {
    if machine::is_running(name) {
        return Err(format!(
            "machine {name:?} is running -- {verb} needs a still machine (stop it or freeze it)"
        ));
    }
    Ok(())
}

fn print_digests(fields: &[(String, String)]) {
    for (k, v) in fields {
        if k.starts_with("digest_") {
            println!("cella:   {k} = {}", &v[..16]);
        }
    }
}

/// branch <existing-vm> <new-vm>: the copy of a still machine. A
/// frozen source yields a frozen twin (the sidecar copies; each
/// twin thaws once), a stopped source a fresh-bootable copy, and a
/// rock copies to a rock: the archived latch carries, because a
/// branch must not resurrect by side effect. The copy carries net
/// none -- the network identity of the source lives in its RAM,
/// and a tap is a deliberate re-attachment. The manifest of the
/// copy records the layer digests of the fork instant.
pub fn branch(src: &str, dst: &str) -> Result<(), String> {
    if !machine::valid_name(dst) {
        return Err(format!(
            "invalid machine name {dst:?}: lowercase letters, digits, and dashes"
        ));
    }
    let src_dir = machine::machine_dir(src);
    if !src_dir.exists() {
        return Err(format!("no machine named {src:?}"));
    }
    refuse_running(src, "branch")?;
    let dst_dir = machine::machine_dir(dst);
    if dst_dir.exists() {
        return Err(format!("machine {dst:?} already exists"));
    }
    let mut m = machine::read_manifest(src)?;
    fs::create_dir_all(&dst_dir).map_err(|e| e.to_string())?;

    // The storage layers, and the sidecar of a frozen source. The
    // console log stays behind: it is the transcript of the source.
    for layer in ["disk.img", "ram.img", "state"] {
        let from = src_dir.join(layer);
        if from.is_file() {
            fs::copy(&from, dst_dir.join(layer)).map_err(|e| format!("copy {layer}: {e}"))?;
        }
    }

    m.name = dst.to_string();
    m.net = "none".to_string();
    let mut fields = layer_digests(&dst_dir)?;
    if machine::is_archived(src) {
        fields.push(("state".to_string(), "archived".to_string()));
    }
    write_manifest_text(&dst_dir, &with_fields(&m.to_json(), &fields))?;

    let kind = if machine::is_archived(dst) {
        "a rock, as the source is (the latch carries)"
    } else if machine::is_frozen(dst) {
        "a frozen twin (each sidecar thaws once)"
    } else {
        "a fresh-bootable copy"
    };
    println!("cella: branched {src:?} -> {dst:?}: {kind}, net none");
    print_digests(&fields);
    Ok(())
}

/// archive <vm>: the machine becomes a rock. The storage layers
/// stay (disk.img, and ram.img where present), the runtime state
/// goes (the sidecar, the transients -- archiving a frozen machine
/// deliberately discards its instant), and the manifest latches
/// state=archived: start, thaw, and enter refuse a rock by name.
pub fn archive(vm: &str) -> Result<(), String> {
    let dir = machine::machine_dir(vm);
    if !dir.exists() {
        return Err(format!("no machine named {vm:?}"));
    }
    refuse_running(vm, "archive")?;
    if machine::is_archived(vm) {
        println!("cella: machine {vm:?} is already a rock");
        return Ok(());
    }
    let m = machine::read_manifest(vm)?;
    for f in ["state", "pid", "console.sock"] {
        let _ = fs::remove_file(dir.join(f));
    }
    let mut fields = layer_digests(&dir)?;
    fields.push(("state".to_string(), "archived".to_string()));
    write_manifest_text(&dir, &with_fields(&m.to_json(), &fields))?;
    println!("cella: archived {vm:?}: a rock (storage layers and digests; nothing resumes)");
    print_digests(&fields);
    Ok(())
}

/// inspect <vm>: attach the disk of a still machine as evidence. A
/// temporary appliance named <vm>-inspector boots the stock rootfs
/// with the machine's disk as a second virtio-blk, read-only at
/// the device; the guest init mounts it at /rock with
/// ro,noexec,nosuid,nodev,norecovery (a frozen source carries an
/// unreplayed journal, and the view is its crash-consistent
/// instant). The terminal attaches; the detach destroys the
/// inspector. The source never changes: a frozen source stays
/// thaw-able, a rock stays a rock.
pub fn inspect(vm: &str) -> Result<(), String> {
    if !machine::machine_dir(vm).exists() {
        return Err(format!("no machine named {vm:?}"));
    }
    refuse_running(vm, "inspect")?;
    let inspector = format!("{vm}-inspector");
    // A stale inspector from an interrupted run goes away first.
    if machine::machine_dir(&inspector).exists() {
        if machine::is_running(&inspector) {
            machine::stop(&inspector)?;
        }
        machine::destroy(&inspector)?;
    }
    let rock_disk = machine::machine_dir(vm).join("disk.img");
    let mut m = machine::defaults();
    m.name = inspector.clone();
    m.attach = rock_disk.to_str().unwrap().to_string();
    machine::create(&m)?;
    machine::start(&inspector)?;
    println!(
        "cella: inspecting {vm:?} -- the evidence is at /rock, read-only, noexec \
         (the detach destroys the inspector)"
    );
    let entered = machine::enter(&inspector);
    let _ = machine::stop(&inspector);
    let _ = machine::destroy(&inspector);
    entered
}
