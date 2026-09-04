//! The bridge loop: ledger out, decisions in, the kick between.
//! One stream per machine, machine-lifetime like the translator
//! (N.T.1): the harness spawns it, and it exits when its machine
//! directory is gone (the tether) or its stream ends.

use crate::pb;
use prost::Message as _;
use std::io::Read;
use std::path::PathBuf;

fn machine_dir(vm: &str) -> PathBuf {
    cella_libs::machine::machine_dir(vm)
}

/// Decode every length-delimited Message in `bytes`, returning the
/// Events. The file holds cella_libs-generated frames; this crate's
/// generated types read the same wire bytes -- one proto, one form.
fn events_in(bytes: &[u8]) -> Vec<pb::Event> {
    let mut out = Vec::new();
    let mut buf = bytes;
    while !buf.is_empty() {
        let before = buf.len();
        match pb::Message::decode_length_delimited(&mut buf) {
            Ok(m) => {
                if let Some(pb::message::Body::Event(e)) = m.body {
                    out.push(e);
                }
            }
            Err(_) => break,
        }
        if buf.len() == before {
            break;
        }
    }
    out
}

/// Append one Decision to the verdict file (N.F.2) and kick the
/// running VMM by SIGWINCH -- the same act the gateway CLI
/// performs, fed from the stream instead of argv. The audit is
/// symmetric (docs/WORLD-ENGINE.md, "Audit"): one witnessed entry
/// per landed decision, the same shape as an operator's release.
fn land(vm: &str, d: pb::Decision) -> Result<(), String> {
    let hex: String = d.id.iter().map(|b| format!("{b:02x}")).collect();
    let word = match &d.decision {
        Some(pb::decision::Decision::Release(_)) => "release",
        Some(pb::decision::Decision::Refusal(_)) => "refuse",
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
    if let Ok(pid) = std::fs::read_to_string(machine_dir(vm).join("pid")) {
        if let Ok(pid) = pid.trim().parse::<i32>() {
            // SAFETY: the machine's own pid file; SIGWINCH is the kick.
            unsafe { libc::kill(pid, libc::SIGWINCH) };
        }
    }
    Ok(())
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

        // The tail: poll the ledger for new frames, send each new
        // Event once. The offset is the cursor; the file is
        // append-only, thus the cursor never rewinds.
        let ledger2 = ledger.clone();
        let dir2 = dir.clone();
        let tail = tokio::spawn(async move {
            let mut sent = 0usize;
            loop {
                // The tether: the machine directory is the lease.
                if !dir2.exists() {
                    break;
                }
                if let Ok(mut f) = std::fs::File::open(&ledger2) {
                    let mut bytes = Vec::new();
                    if f.read_to_end(&mut bytes).is_ok() {
                        let events = events_in(&bytes);
                        for e in events.iter().skip(sent) {
                            if tx.send(e.clone()).await.is_err() {
                                return;
                            }
                        }
                        sent = events.len();
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        });

        // Decisions land as they arrive. The bridge never filters,
        // reorders, or defaults: the engine's word, verbatim.
        while let Some(d) = inbound
            .message()
            .await
            .map_err(|e| format!("stream: {e}"))?
        {
            land(&vm_name, d)?;
        }
        tail.abort();
        Ok(())
    })
}
