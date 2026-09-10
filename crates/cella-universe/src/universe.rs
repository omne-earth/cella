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

    // The books (1.6.14d): a byte-identical copy carries the chain
    // as it stands -- the twin forks with the source's history, and
    // both books verify from the same genesis onward. Copied, never
    // rewritten: the predecessor field of the twin's first new
    // entry still names the copied tail, same as the source's would
    // have.
    let ledger_from = src_dir.join("network").join("ledger");
    if ledger_from.is_file() {
        let ledger_dir = dst_dir.join("network");
        fs::create_dir_all(&ledger_dir).map_err(|e| format!("mkdir network: {e}"))?;
        fs::copy(&ledger_from, ledger_dir.join("ledger"))
            .map_err(|e| format!("copy ledger: {e}"))?;
    }
    let audit_from = src_dir.join("audit");
    if audit_from.is_file() {
        fs::copy(&audit_from, dst_dir.join("audit")).map_err(|e| format!("copy audit: {e}"))?;
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
/// The release build carries no interactive inspect at all: the
/// attach rides the console, which the release binary is built
/// without. This stub is the whole of inspect there -- the refused
/// attempt is still witnessed (main.rs), and no appliance spins up.
#[cfg(not(debug_assertions))]
pub fn inspect(_vm: &str) -> Result<(), String> {
    // One verbatim message for every lab-only verb (enter, inspect):
    // keep the two stubs in sync.
    Err(
        "only available for lab installs, use cella extract <machine> <path> \
         instead. To install lab version, run scripts/setup/install.sh --lab. \
         Then run cella-debug enter|inspect <vm>"
            .to_string(),
    )
}

#[cfg(debug_assertions)]
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

/// extract <vm> <guest-path>: copy evidence out of a still machine
/// as a tar stream on stdout. The mechanism is inspect's appliance
/// without the human: a temporary machine named <vm>-extractor
/// boots the stock rootfs with the evidence at /rock and a blank
/// scratch disk as a third virtio-blk; its init tars the named path
/// onto the raw scratch, writes a trailer (byte length + sha256, or
/// the failure's reason) to sector 0 last, and halts. The host
/// polls for the trailer, stops the appliance, checks the digest,
/// and streams -- a missing or wrong trailer is an error, never a
/// truncated tar passed off as evidence. No console takes part: the
/// verb works in the field flavor. The read is witnessed like every
/// verb (main.rs).
pub fn extract(vm: &str, path: &str) -> Result<(), String> {
    if !machine::machine_dir(vm).exists() {
        return Err(format!("no machine named {vm:?}"));
    }
    refuse_running(vm, "extract")?;
    if !path.starts_with('/') {
        return Err(format!("the guest path must be absolute: {path:?}"));
    }
    if path.contains(char::is_whitespace) {
        // The path rides the kernel command line.
        return Err(format!("the guest path cannot contain spaces: {path:?}"));
    }
    let extractor = format!("{vm}-extractor");
    // A stale extractor from an interrupted run goes away first.
    if machine::machine_dir(&extractor).exists() {
        if machine::is_running(&extractor) {
            machine::stop(&extractor)?;
        }
        machine::destroy(&extractor)?;
    }
    // stdout is the tar and nothing else: the lifecycle prints of
    // create/start/destroy move to stderr for the whole verb, and
    // the stream writes to the saved descriptor directly.
    // SAFETY: dup/dup2 on the process's own standard descriptors.
    let saved_stdout = unsafe { libc::dup(1) };
    if saved_stdout < 0 {
        return Err("saving stdout failed".to_string());
    }
    unsafe { libc::dup2(2, 1) };
    let restore = |fd: i32| {
        // SAFETY: restoring the saved descriptor.
        unsafe {
            libc::dup2(fd, 1);
            libc::close(fd);
        }
    };
    let evidence = machine::machine_dir(vm).join("disk.img");
    let evidence_len = match fs::metadata(&evidence) {
        Ok(md) => md.len(),
        Err(e) => {
            restore(saved_stdout);
            return Err(e.to_string());
        }
    };
    let scratch = machine::machine_dir(&extractor).join("scratch.img");
    let mut m = machine::defaults();
    m.name = extractor.clone();
    m.attach = evidence.to_str().unwrap().to_string();
    m.scratch = scratch.to_str().unwrap().to_string();
    m.extract = path.to_string();
    machine::create(&m)?;
    // The scratch: sparse, sized for the worst case -- a tar of the
    // whole evidence plus headers -- and the 512-byte trailer sector.
    let scratch_len = evidence_len + evidence_len / 8 + (1 << 20) + 512;
    let f = fs::File::create(&scratch).map_err(|e| format!("creating the scratch: {e}"))?;
    f.set_len(scratch_len).map_err(|e| e.to_string())?;
    drop(f);
    let done = (|| -> Result<(), String> {
        machine::start(&extractor)?;
        // The job's end is the trailer on the scratch -- a fact on
        // disk, not a message. The guest resets when it finishes,
        // but a reset is not a reliable exit (the kernel may boot
        // again instead of shutting the VMM down), thus the host
        // polls the trailer and stops the appliance itself. The
        // budget scales with the evidence.
        let budget = std::time::Duration::from_secs(60 + evidence_len / (4 << 20));
        let t0 = std::time::Instant::now();
        loop {
            if trailer_present(&scratch) {
                let _ = machine::stop(&extractor);
                break;
            }
            if !machine::is_running(&extractor) {
                // The appliance ended on its own: the trailer check
                // below decides whether the job finished first.
                break;
            }
            if t0.elapsed() > budget {
                let _ = machine::stop(&extractor);
                return Err(format!(
                    "the extractor did not finish within {}s",
                    budget.as_secs()
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        stream_scratch(&scratch, saved_stdout)
    })();
    let _ = machine::destroy(&extractor);
    restore(saved_stdout);
    done
}

/// Does sector 0 of the scratch carry the trailer's magic yet? The
/// guest writes the trailer last, in one 512-byte write; its
/// presence means the tar stands complete. The full parse and the
/// digest check happen in stream_scratch.
fn trailer_present(scratch: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = fs::File::open(scratch) else {
        return false;
    };
    let mut magic = [0u8; 14];
    // Either verdict: "cella-extract-1" is a finished tar,
    // "cella-extract-0" a job that failed and says why.
    f.read_exact(&mut magic).is_ok() && &magic == b"cella-extract-"
}

/// Verify the trailer of a finished extract and stream the tar to
/// the saved stdout descriptor (fd 1 carries the lifecycle prints
/// during the verb). Sector 0 carries "cella-extract-1 <len>
/// <sha256>"; the tar's bytes start at offset 512. The digest is
/// checked before one byte leaves.
fn stream_scratch(scratch: &Path, out_fd: i32) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = fs::File::open(scratch).map_err(|e| e.to_string())?;
    let mut sector = [0u8; 512];
    f.read_exact(&mut sector).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&sector);
    let text = text.trim_end_matches('\0');
    let mut words = text.split_whitespace();
    match words.next() {
        Some("cella-extract-1") => {}
        Some("cella-extract-0") => {
            let why = text["cella-extract-0".len()..].trim().trim_end();
            return Err(format!("the extract job failed in the guest: {why}"));
        }
        _ => {
            return Err(
                "the extractor left no trailer -- the job died before it finished".to_string(),
            )
        }
    }
    let len: u64 = words
        .next()
        .and_then(|w| w.parse().ok())
        .ok_or("the trailer names no length")?;
    let sum = words.next().ok_or("the trailer names no digest")?;
    // Pass one: the digest, before anything streams.
    f.seek(SeekFrom::Start(512)).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut left = len;
    let mut buf = vec![0u8; 1 << 20];
    while left > 0 {
        let n = buf.len().min(left as usize);
        f.read_exact(&mut buf[..n]).map_err(|e| e.to_string())?;
        hasher.update(&buf[..n]);
        left -= n as u64;
    }
    let got = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if got != sum {
        return Err(format!(
            "the tar does not match its trailer (trailer {sum}, read {got}) -- \
             the scratch is corrupt, nothing streams"
        ));
    }
    // Pass two: the bytes, to the saved descriptor.
    f.seek(SeekFrom::Start(512)).map_err(|e| e.to_string())?;
    // SAFETY: a fresh dup of the saved stdout; the File owns and
    // closes the duplicate, never the original.
    let dup = unsafe { libc::dup(out_fd) };
    if dup < 0 {
        return Err("duplicating the output descriptor failed".to_string());
    }
    // SAFETY: dup is a valid, owned descriptor.
    let mut out = unsafe { <fs::File as std::os::fd::FromRawFd>::from_raw_fd(dup) };
    let mut left = len;
    while left > 0 {
        let n = buf.len().min(left as usize);
        f.read_exact(&mut buf[..n]).map_err(|e| e.to_string())?;
        out.write_all(&buf[..n])
            .map_err(|e| format!("writing stdout: {e}"))?;
        left -= n as u64;
    }
    out.flush().map_err(|e| e.to_string())?;
    Ok(())
}
