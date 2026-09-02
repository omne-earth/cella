//! cella gateway: the membrane's surface. See docs/NETWORK-MODEL.md,
//! "The control plane".
//!
//! show reads the chronicle; release and refuse append Decisions and
//! kick a running machine (a frozen one applies them at thaw); close
//! shuts the valve; open states the one-way rule and refuses. Every
//! verb is unprivileged: files and signals on the machine's own
//! directory, never a capability.

use std::fs;
use std::path::PathBuf;

use cella_libs::{freeze, ledger, proto};

// The gateway carries no machine code (1.6.13): the <vm>/ directory
// is its complete interface -- five files plus SIGWINCH -- and these
// path helpers are the whole of its registry knowledge. The seam
// between the personas is a directory of framed files and a signal:
// a protocol, never an API.
fn home() -> PathBuf {
    if let Ok(h) = std::env::var("CELLA_HOME") {
        return PathBuf::from(h);
    }
    let base = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join(".cella")
}

fn machine_dir(vm: &str) -> PathBuf {
    home().join("machines").join(vm)
}

/// The VMM stands when its pid file names a live process.
fn is_running(vm: &str) -> bool {
    let Ok(pid) = fs::read_to_string(machine_dir(vm).join("pid")) else {
        return false;
    };
    let Ok(pid) = pid.trim().parse::<i32>() else {
        return false;
    };
    // SAFETY: signal 0 probes existence and delivers nothing.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The valve automaton's record: one word beside the machine, the
/// gateway's own to write (docs/FREEZE-THAW.md, "The two automata").
fn set_valve_record(vm: &str, word: &str) -> Result<(), String> {
    let p = machine_dir(vm).join("valve");
    let tmp = machine_dir(vm).join("valve.tmp");
    fs::write(&tmp, format!("{word}\n")).map_err(|e| format!("writing the valve record: {e}"))?;
    fs::rename(&tmp, &p).map_err(|e| format!("writing the valve record: {e}"))
}

fn ledger_path(vm: &str) -> PathBuf {
    machine_dir(vm).join("network").join("ledger")
}

fn verdict_path(vm: &str) -> PathBuf {
    machine_dir(vm).join("verdict")
}

/// The chronicle, split into what an operator asks about: every
/// parked operation, and the set of ids a decision already resolved.
struct Book {
    parked: Vec<proto::Operation>,
    resolved: Vec<(Vec<u8>, &'static str, String)>, // id, state, detail
}

fn read_book(vm: &str) -> Result<Book, String> {
    let path = ledger_path(vm);
    if !path.is_file() {
        return Ok(Book {
            parked: Vec::new(),
            resolved: Vec::new(),
        });
    }
    let messages = ledger::read_all(&path).map_err(|e| format!("reading the chronicle: {e}"))?;
    let mut book = Book {
        parked: Vec::new(),
        resolved: Vec::new(),
    };
    for msg in messages {
        let Some(proto::message::Body::Event(ev)) = msg.body else {
            continue;
        };
        match ev.event {
            Some(proto::event::Event::Parked(op)) => book.parked.push(op),
            Some(proto::event::Event::Released(r)) => book.resolved.push((
                r.id,
                "released",
                format!("bytes_out={} bytes_in={}", r.bytes_out, r.bytes_in),
            )),
            Some(proto::event::Event::Lapsed(l)) => book.resolved.push((l.id, "lapsed", l.why)),
            // A look resolves nothing: the operation stays held.
            Some(proto::event::Event::Inspected(_)) => {}
            None => {}
        }
    }
    Ok(book)
}

fn is_open(book: &Book, id: &[u8]) -> bool {
    !book.resolved.iter().any(|(rid, _, _)| rid == id)
}

/// Resolve an id prefix (hex) against the open operations, the git
/// convention: an unambiguous prefix names the operation, an
/// ambiguous or unknown one is an error that says so.
fn resolve_id(book: &Book, prefix: &str) -> Result<Vec<u8>, String> {
    let matches: Vec<&proto::Operation> = book
        .parked
        .iter()
        .filter(|op| is_open(book, &op.id) && ledger::hex(&op.id).starts_with(prefix))
        .collect();
    match matches.len() {
        1 => Ok(matches[0].id.clone()),
        0 => Err(format!(
            "no held operation matches {prefix:?} -- cella gateway <vm> show lists them"
        )),
        n => Err(format!(
            "{prefix:?} matches {n} held operations -- more digits"
        )),
    }
}

fn is_incoming(op: &proto::Operation) -> bool {
    op.direction == proto::operation::Direction::Incoming as i32
}

fn fmt_dest(op: &proto::Operation) -> String {
    match &op.destination {
        Some(d) => match ledger::Dest::from_message(d) {
            ledger::Dest::Ipv4 { ip, port, .. } => {
                let ip: Vec<String> = ip.iter().map(u8::to_string).collect();
                if d.host.is_empty() {
                    format!("{}:{}", ip.join("."), port)
                } else {
                    format!("{} ({}:{})", d.host, ip.join("."), port)
                }
            }
            // The L2 name: the ethertype word and the destination
            // MAC (arp ff:ff:ff:ff:ff:ff).
            l2 => l2.to_string(),
        },
        None => "(unknown)".to_string(),
    }
}

fn fmt_age(host_ns: u64) -> String {
    let now = ledger::host_ns_now();
    let secs = now.saturating_sub(host_ns) / 1_000_000_000;
    if secs < 120 {
        format!("{secs}s")
    } else {
        format!("{}m", secs / 60)
    }
}

/// The pid of the running VMM, when one runs.
fn running_pid(vm: &str) -> Option<i32> {
    if !is_running(vm) {
        return None;
    }
    fs::read_to_string(machine_dir(vm).join("pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Each direction has one door and its own wire. An egress
/// decision stages, and the thaw edge alone applies it (under
/// one-shot a running machine froze at its first park and holds no
/// egress). An incoming hold never freezes the machine, thus the
/// ear's door is a live wire: the verb kicks a running machine and
/// the mail moves now; a sleeping one applies at the thaw.
fn decision_note(vm: &str, incoming: bool) {
    if !incoming {
        println!("cella: the decision is staged -- it applies at the thaw, in park order");
        return;
    }
    match running_pid(vm) {
        Some(pid) => {
            // SAFETY: the pid comes from the machine's own pid file.
            unsafe { libc::kill(pid, libc::SIGWINCH) };
            println!("cella: the incoming decision applies now (the machine runs)");
        }
        None => println!("cella: the incoming decision applies at the thaw, in park order"),
    }
}

fn append_decision(vm: &str, id: Vec<u8>, d: proto::decision::Decision) -> Result<(), String> {
    let msg = proto::Message {
        body: Some(proto::message::Body::Decision(proto::Decision {
            id,
            decision: Some(d),
        })),
    };
    ledger::append(&verdict_path(vm), &msg).map_err(|e| format!("writing the decision: {e}"))
}

/// show renders the border's book. Bare show carries both
/// directions with a DIRECTION and a neutral PEER column; a
/// direction narrows it, and the column names the side honestly:
/// DESTINATION for outgoing, SOURCE for incoming.
pub fn show(vm: &str, all: bool, direction: Option<&str>) -> Result<(), String> {
    if !machine_dir(vm).exists() {
        return Err(format!("no machine named {vm:?}"));
    }
    let book = read_book(vm)?;
    match direction {
        None => println!(
            "{:<34} {:<9} {:<40} {:>6}  STATE",
            "OPERATION", "DIRECTION", "PEER", "AGE"
        ),
        Some("incoming") => println!("{:<34} {:<40} {:>6}  STATE", "OPERATION", "SOURCE", "AGE"),
        _ => println!(
            "{:<34} {:<40} {:>6}  STATE",
            "OPERATION", "DESTINATION", "AGE"
        ),
    }
    let mut held = 0;
    for op in &book.parked {
        let open = is_open(&book, &op.id);
        if !open && !all {
            continue;
        }
        let incoming = is_incoming(op);
        match direction {
            Some("incoming") if !incoming => continue,
            Some("outgoing") if incoming => continue,
            _ => {}
        }
        let state = if open {
            held += 1;
            "held".to_string()
        } else {
            let (_, s, detail) = book
                .resolved
                .iter()
                .find(|(rid, _, _)| rid == &op.id)
                .expect("resolved contains every non-open id");
            format!("{s} ({detail})")
        };
        match direction {
            None => println!(
                "{:<34} {:<9} {:<40} {:>6}  {}",
                ledger::hex(&op.id),
                if incoming { "incoming" } else { "outgoing" },
                fmt_dest(op),
                fmt_age(op.host_ns),
                state
            ),
            _ => println!(
                "{:<34} {:<40} {:>6}  {}",
                ledger::hex(&op.id),
                fmt_dest(op),
                fmt_age(op.host_ns),
                state
            ),
        }
    }
    if held == 0 {
        println!("(no held operations)");
    }
    Ok(())
}

pub fn release(vm: &str, prefix: &str) -> Result<(), String> {
    let book = read_book(vm)?;
    let id = resolve_id(&book, prefix)?;
    let incoming = book
        .parked
        .iter()
        .find(|op| op.id == id)
        .map(is_incoming)
        .unwrap_or(false);
    append_decision(
        vm,
        id.clone(),
        proto::decision::Decision::Release(proto::Release {}),
    )?;
    let dir = if incoming { "incoming" } else { "outgoing" };
    println!("cella: release {} ({dir})", ledger::hex(&id));
    decision_note(vm, incoming);
    Ok(())
}

pub fn refuse(vm: &str, prefix: &str, why: &str) -> Result<(), String> {
    let book = read_book(vm)?;
    let id = resolve_id(&book, prefix)?;
    let incoming = book
        .parked
        .iter()
        .find(|op| op.id == id)
        .map(is_incoming)
        .unwrap_or(false);
    append_decision(
        vm,
        id.clone(),
        proto::decision::Decision::Refusal(proto::Refusal {
            why: why.to_string(),
        }),
    )?;
    println!("cella: refuse {} ({why})", ledger::hex(&id));
    decision_note(vm, incoming);
    Ok(())
}

/// Set the valve posture. The valve automaton's record lives
/// beside the machine (never in the manifest), the gateway verbs
/// alone write it, and the posture holds across any number of
/// freezes and thaws until the opposite verb. A running machine is
/// kicked so the posture applies now; a sleeping one reads the
/// record when it next runs.
fn set_posture(vm: &str, word: &str) -> Result<(), String> {
    if !machine_dir(vm).exists() {
        return Err(format!("no machine named {vm:?}"));
    }
    set_valve_record(vm, word)?;
    match running_pid(vm) {
        Some(pid) => {
            // SAFETY: the pid comes from the machine's own pid file.
            unsafe { libc::kill(pid, libc::SIGWINCH) };
            println!("cella: the valve of {vm:?} is {word}, now");
        }
        None => println!(
            "cella: the valve of {vm:?} is {word} -- the posture holds across freeze and thaw"
        ),
    }
    Ok(())
}

/// close: the closed machine. Nothing goes in or out -- no parking, no
/// ledger, no freeze; the machine runs dark.
pub fn close(vm: &str) -> Result<(), String> {
    set_posture(vm, "closed")
}

/// open: the membrane, never a free flow. Egress reaches the hold
/// trigger: initiations and replies alike park, and the park is the
/// freeze; only decisions let anything through.
pub fn open(vm: &str) -> Result<(), String> {
    set_posture(vm, "open")
}

/// The Shannon entropy of a byte slice, in bits per byte. The
/// sealed-envelope heuristic: an encrypted payload is
/// indistinguishable from randomness, and rendering it as hex
/// pretends a sight that does not exist.
fn entropy_bits(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for b in bytes {
        counts[*b as usize] += 1;
    }
    let n = bytes.len() as f64;
    counts
        .iter()
        .filter(|c| **c > 0)
        .map(|c| {
            let p = *c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// One frame as hex and ascii, sixteen bytes per line.
fn render_frame(frame: &[u8]) {
    for (i, chunk) in frame.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|b| {
                if b.is_ascii_graphic() || *b == b' ' {
                    *b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("    {:04x}  {:<47}  {}", i * 16, hex.join(" "), ascii);
    }
}

/// cella gateway <vm> inspect <id>: the operator reads a held
/// operation's frames. Frozen-only (the ruling of 1.6.10): sight
/// requires stillness -- a running lane mutates under the render,
/// and evidence-grade means a consistent instant. The verb reads
/// the sidecar alone (the vessel; the ledger is a chronicle, never
/// the store), matches frames to the operation by its primitive
/// key with the never-guess rule, renders an encrypted payload as
/// the sealed envelope it is, and records the look: an Inspected
/// event lands in the chronicle. The look resolves nothing and
/// changes no state in either automaton.
pub fn inspect(vm: &str, prefix: &str) -> Result<(), String> {
    if !machine_dir(vm).exists() {
        return Err(format!("no machine named {vm:?}"));
    }
    let book = read_book(vm)?;
    let id = resolve_id(&book, prefix)?;
    let op = book
        .parked
        .iter()
        .find(|op| op.id == id)
        .expect("resolve_id returned a parked id");
    let key = ledger::Dest::from_message(&op.destination.clone().unwrap_or_default());
    let incoming = is_incoming(op);
    // The never-guess rule, at the read: two open operations under
    // one key make every frame ambiguous, and ambiguity is no
    // sight.
    let twins = book
        .parked
        .iter()
        .filter(|o| {
            is_open(&book, &o.id)
                && is_incoming(o) == incoming
                && ledger::Dest::from_message(&o.destination.clone().unwrap_or_default()) == key
        })
        .count();
    if twins > 1 {
        return Err(format!(
            "{twins} held operations share this key -- the matcher never guesses: refuse the stale ones first"
        ));
    }
    if is_running(vm) {
        return Err(
            "sight requires stillness -- a running lane mutates under the render: \
             freeze first (cella freeze <vm>), and the look is itself witnessed"
                .to_string(),
        );
    }
    let dir = machine_dir(vm);
    if !freeze::is_frozen(&dir) {
        return Err(
            "the machine is stopped and holds nothing -- a held operation's frames \
             live in a frozen machine's sidecar"
                .to_string(),
        );
    }
    let st = freeze::read_state(&dir).map_err(|e| format!("reading the sidecar: {e:?}"))?;
    let mut frames: Vec<&Vec<u8>> = Vec::new();
    let mut heads: Vec<Vec<u8>> = Vec::new();
    for t in &st.devices {
        if incoming {
            for f in &t.ingress_held {
                if ledger::frame_source_name(f) == key {
                    frames.push(f);
                }
            }
        } else {
            for (_, f) in &t.held_frames {
                if ledger::frame_dest_name(f) == key {
                    heads.push(f.clone());
                }
            }
        }
    }
    let owned: Vec<&Vec<u8>> = if incoming {
        frames
    } else {
        heads.iter().collect()
    };
    if owned.is_empty() {
        return Err("the sidecar holds no frames under this operation's key".to_string());
    }
    let total: usize = owned.iter().map(|f| f.len()).sum();
    println!(
        "operation {}  {}  peer {}  {} frame(s)  {} byte(s)",
        ledger::hex(&id),
        if incoming { "incoming" } else { "outgoing" },
        key,
        owned.len(),
        total
    );
    for (i, f) in owned.iter().enumerate() {
        // The 12-byte vnet header is virtio plumbing, not the wire.
        let wire = f.get(12..).unwrap_or(&[]);
        let e = entropy_bits(wire);
        if wire.len() >= 128 && e > 7.0 {
            println!(
                "  frame {}: {} bytes -- a sealed envelope ({e:.2} bits/byte); \
                 the terminator opens these",
                i + 1,
                wire.len()
            );
            continue;
        }
        println!("  frame {}: {} bytes", i + 1, wire.len());
        render_frame(wire);
    }
    // The look is itself recorded. The machine is frozen, thus no
    // other writer holds the chronicle.
    let msg = ledger::event_message(proto::Event {
        event: Some(proto::event::Event::Inspected(proto::Inspected {
            id: id.clone(),
        })),
    });
    ledger::append(&ledger_path(vm), &msg).map_err(|e| format!("writing the look: {e}"))?;
    println!(
        "cella: inspected {} -- the look is in the chronicle",
        ledger::hex(&id)
    );
    Ok(())
}
