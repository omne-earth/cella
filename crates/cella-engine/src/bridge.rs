//! The bridge loop: ledger out, decisions in, the kick between.
//! One stream per machine, machine-lifetime like the translator
//! (N.T.1): the harness spawns it, and it exits when its machine
//! directory is gone (the tether) or its stream ends.

use crate::pb;
use prost::Message as _;
use std::io::Read;
use std::path::{Path, PathBuf};

fn machine_dir(vm: &str) -> PathBuf {
    cella_libs::machine::machine_dir(vm)
}

/// Append one Decision to the verdict file (N.F.2) and kick the
/// running VMM by SIGWINCH -- the same act the gateway CLI
/// performs, fed from the stream instead of argv. The audit is
/// symmetric (docs/WORLD-ENGINE.md, "Audit"): one witnessed entry
/// per landed decision, the same shape as an operator's release.
fn land(vm: &str, d: pb::Decision) -> Result<(), String> {
    // A membrane memory is not a verdict on a hold: it lands in the
    // machine's membrane-memory file (N.F.7), never the verdict.
    if let Some(pb::decision::Decision::MembraneMemory(m)) = &d.decision {
        return land_memory(vm, m);
    }
    let hex: String = d.id.iter().map(|b| format!("{b:02x}")).collect();
    let word = match &d.decision {
        Some(pb::decision::Decision::Release(_)) => "release",
        Some(pb::decision::Decision::Refusal(_)) => "refuse",
        Some(pb::decision::Decision::MembraneMemory(_)) => unreachable!(),
        None => "decision",
    };
    cella_libs::audit::witness(Some(vm), word, &[hex])
        .map_err(|e| format!("witnessing the decision: {e}"))?;
    let msg = pb::Message {
        body: Some(pb::message::Body::Decision(d)),
    };
    let mut buf = Vec::with_capacity(msg.encoded_len() + 4);
    msg.encode_length_delimited(&mut buf)
        .map_err(|e| e.to_string())?;
    let path = machine_dir(vm).join("verdict");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("appending {path:?}: {e}"))?;
    f.write_all(&buf).map_err(|e| e.to_string())?;
    kick(vm);
    Ok(())
}

/// Land one standing memory: stamp `written` with the host clock
/// (the judge's time -- expiry is absolute from here), witness the
/// landing, append to the membrane-memory file, and kick. The file
/// is append-only forever: every byte in it is a ruling the judge
/// chose to make.
fn land_memory(vm: &str, m: &pb::MembraneMemory) -> Result<(), String> {
    let mut m = m.clone();
    m.written = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dest = match &m.destination {
        Some(d) if !d.ip.is_empty() => format!(
            "{}:{}/{}",
            d.ip.iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join("."),
            d.port,
            d.proto
        ),
        Some(d) => format!("0x{:04x}", d.ethertype),
        None => "unnamed".to_string(),
    };
    cella_libs::audit::witness(
        Some(vm),
        "membrane-memory",
        &[dest, format!("keep_open={}s", m.keep_open)],
    )
    .map_err(|e| format!("witnessing the landing: {e}"))?;
    // This crate's generated type writes the same wire bytes as
    // cella_libs' (one proto, one form): frame it directly,
    // valve-style, no Message envelope.
    let mut buf = Vec::with_capacity(m.encoded_len() + 4);
    m.encode_length_delimited(&mut buf)
        .map_err(|e| e.to_string())?;
    let path = machine_dir(vm).join("membrane-memory");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("appending {path:?}: {e}"))?;
    f.write_all(&buf).map_err(|e| e.to_string())?;
    kick(vm);
    Ok(())
}

/// The kick: SIGWINCH to the machine's own pid, the live wire.
fn kick(vm: &str) {
    if let Ok(pid) = std::fs::read_to_string(machine_dir(vm).join("pid")) {
        if let Ok(pid) = pid.trim().parse::<i32>() {
            // SAFETY: the machine's own pid file; SIGWINCH is the kick.
            unsafe { libc::kill(pid, libc::SIGWINCH) };
        }
    }
}

pub fn run(vm: &str, dial: &str) -> Result<(), String> {
    let dir = machine_dir(vm);
    if !dir.exists() {
        return Err(format!("no machine named {vm:?}"));
    }
    let ledger = dir.join("network/ledger");
    let vm_name = vm.to_string();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let endpoint = format!("http://{dial}");
        let mut client = pb::engine_client::EngineClient::connect(endpoint)
            .await
            .map_err(|e| format!("dialing {dial}: {e}"))?;
        let (tx, rx) = tokio::sync::mpsc::channel::<pb::Event>(64);
        let outbound = tokio_stream::wrappers::ReceiverStream::new(rx);
        let mut inbound = client
            .decide(outbound)
            .await
            .map_err(|e| format!("Decide: {e}"))?
            .into_inner();

        // The tail: an ear on the ledger, not a poll. A sustained
        // flow is one operation per verdict round trip, so the
        // tail's latency is the pair's throughput ceiling -- a
        // 200 ms poll clocked bulk transfers at tens of kB/s. The
        // tail therefore wakes on inotify (a 500 ms poll stands
        // behind it as the safety net, and becomes the pace only
        // when inotify is unavailable), reads incrementally from a
        // byte cursor -- append-only, thus the cursor never
        // rewinds -- and a torn final frame waits for its missing
        // bytes rather than being re-read whole.
        let ledger2 = ledger.clone();
        let dir2 = dir.clone();
        std::thread::spawn(move || tail_thread(&dir2, &ledger2, tx));

        // Decisions land as they arrive. The bridge never filters,
        // reorders, or defaults: the engine's word, verbatim.
        cella_libs::logln!("bridge: {vm_name} connected to {dial}");
        while let Some(d) = inbound
            .message()
            .await
            .map_err(|e| format!("stream: {e}"))?
        {
            land(&vm_name, d)?;
        }
        cella_libs::logln!("bridge: {vm_name} stream ended");
        Ok(())
    })
    // The tail thread ends on its own: the runtime drop closes the
    // channel, and the next send fails; the tether covers the rest.
}

/// The tail's body: wake, read the new bytes, frame them, send.
fn tail_thread(dir: &Path, ledger: &Path, tx: tokio::sync::mpsc::Sender<pb::Event>) {
    // The ear: inotify on the ledger's parent directory (the file
    // may not exist yet at the first wake). Any write in the
    // directory wakes the tail; a wake is one cheap metadata read.
    // The directory itself is born with the VMM's first flush, so
    // the watch retries until it takes -- until then the loop
    // paces itself.
    let ifd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    let listen = |ifd: i32| -> bool {
        if ifd < 0 {
            return false;
        }
        let Some(parent) = ledger.parent() else {
            return false;
        };
        let Ok(c) = std::ffi::CString::new(parent.as_os_str().as_encoded_bytes()) else {
            return false;
        };
        // SAFETY: a valid fd and a NUL-terminated path.
        let wd =
            unsafe { libc::inotify_add_watch(ifd, c.as_ptr(), libc::IN_MODIFY | libc::IN_CREATE) };
        wd >= 0
    };
    let mut heard = listen(ifd);
    let mut offset: u64 = 0;
    let mut pending: Vec<u8> = Vec::new();
    loop {
        // The tether: the machine directory is the lease.
        if !dir.exists() {
            break;
        }
        if !heard {
            heard = listen(ifd);
            if heard {
                cella_libs::logln!("bridge: the tail hears the ledger");
            }
        }
        if let Ok(mut f) = std::fs::File::open(ledger) {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            if len > offset {
                use std::io::Seek;
                let mut chunk = vec![0u8; (len - offset) as usize];
                if f.seek(std::io::SeekFrom::Start(offset)).is_ok()
                    && f.read_exact(&mut chunk).is_ok()
                {
                    offset = len;
                    pending.extend_from_slice(&chunk);
                    let mut consumed = 0usize;
                    loop {
                        let mut buf = &pending[consumed..];
                        let before = buf.len();
                        match pb::Message::decode_length_delimited(&mut buf) {
                            Ok(m) => {
                                let used = before - buf.len();
                                if used == 0 {
                                    break;
                                }
                                consumed += used;
                                if let Some(pb::message::Body::Event(e)) = m.body {
                                    if tx.blocking_send(e).is_err() {
                                        if ifd >= 0 {
                                            // SAFETY: our own fd.
                                            unsafe { libc::close(ifd) };
                                        }
                                        return;
                                    }
                                }
                            }
                            // A torn final frame: keep the bytes,
                            // wait for the rest of them.
                            Err(_) => break,
                        }
                    }
                    pending.drain(..consumed);
                }
            }
        }
        if heard {
            let mut pfd = libc::pollfd {
                fd: ifd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd; the timeout is the safety
            // net against a missed event, never the pace.
            if unsafe { libc::poll(&mut pfd, 1, 500) } > 0 {
                let mut evbuf = [0u8; 4096];
                // SAFETY: draining our own fd into a local buffer.
                let _ = unsafe { libc::read(ifd, evbuf.as_mut_ptr().cast(), evbuf.len()) };
            }
        } else {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    if ifd >= 0 {
        // SAFETY: our own fd.
        unsafe { libc::close(ifd) };
    }
}
