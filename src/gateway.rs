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

use crate::{ledger, machine, proto};

fn ledger_path(vm: &str) -> PathBuf {
    machine::machine_dir(vm).join("network").join("ledger")
}

fn verdict_path(vm: &str) -> PathBuf {
    machine::machine_dir(vm).join("verdict")
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

fn fmt_dest(op: &proto::Operation) -> String {
    match &op.destination {
        Some(d) => {
            let ip: Vec<String> = d.ip.iter().map(u8::to_string).collect();
            if d.host.is_empty() {
                format!("{}:{}", ip.join("."), d.port)
            } else {
                format!("{} ({}:{})", d.host, ip.join("."), d.port)
            }
        }
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
    if !machine::is_running(vm) {
        return None;
    }
    fs::read_to_string(machine::machine_dir(vm).join("pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn kick_or_wait(vm: &str) {
    match running_pid(vm) {
        Some(pid) => {
            // SAFETY: the pid comes from the machine's own pid file.
            unsafe { libc::kill(pid, libc::SIGWINCH) };
            println!("cella: the decision applies now (the machine runs)");
        }
        None => {
            println!("cella: the machine sleeps -- the decision applies at thaw, in park order");
        }
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

pub fn show(vm: &str, all: bool) -> Result<(), String> {
    if !machine::machine_dir(vm).exists() {
        return Err(format!("no machine named {vm:?}"));
    }
    let book = read_book(vm)?;
    println!(
        "{:<34} {:<40} {:>6}  {}",
        "OPERATION", "DESTINATION", "AGE", "STATE"
    );
    let mut held = 0;
    for op in &book.parked {
        let open = is_open(&book, &op.id);
        if !open && !all {
            continue;
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
        println!(
            "{:<34} {:<40} {:>6}  {}",
            ledger::hex(&op.id),
            fmt_dest(op),
            fmt_age(op.host_ns),
            state
        );
    }
    if held == 0 {
        println!("(no held operations)");
    }
    Ok(())
}

pub fn release(vm: &str, prefix: &str, allow: bool) -> Result<(), String> {
    let book = read_book(vm)?;
    let id = resolve_id(&book, prefix)?;
    append_decision(
        vm,
        id.clone(),
        proto::decision::Decision::Release(proto::Release { allow_flow: allow }),
    )?;
    println!("cella: release {} (allow_flow: {allow})", ledger::hex(&id));
    kick_or_wait(vm);
    Ok(())
}

pub fn refuse(vm: &str, prefix: &str, why: &str) -> Result<(), String> {
    let book = read_book(vm)?;
    let id = resolve_id(&book, prefix)?;
    append_decision(
        vm,
        id.clone(),
        proto::decision::Decision::Refusal(proto::Refusal {
            why: why.to_string(),
        }),
    )?;
    println!("cella: refuse {} ({why})", ledger::hex(&id));
    kick_or_wait(vm);
    Ok(())
}

pub fn close(vm: &str) -> Result<(), String> {
    if ledger_path(vm).is_file() {
        println!("cella: the valve of {vm:?} is already closed (its chronicle exists)");
        return Ok(());
    }
    let Some(pid) = running_pid(vm) else {
        return Err(format!(
            "machine {vm:?} is not running -- the valve closes on a running machine, \
             and stays closed once its chronicle exists"
        ));
    };
    // SAFETY: the pid comes from the machine's own pid file.
    unsafe { libc::kill(pid, libc::SIGUSR2) };
    println!("cella: the valve of {vm:?} is closed -- egress parks, and the park is the freeze");
    Ok(())
}

pub fn open(vm: &str) -> Result<(), String> {
    Err(format!(
        "the valve ratchets one way: {vm:?} cannot reopen. A held operation wants a \
         decision (release or refuse, by id); an attended posture does not exist"
    ))
}
