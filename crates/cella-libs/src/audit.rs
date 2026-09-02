//! The witnessed border: every verb is an event, show and inspect
//! included -- every human action is as auditable as the machine's.
//! The chronicle stays the operations ledger; the verbs get their
//! own audit stream in the same proto language (see
//! proto/cella.proto, Audit). The operator acts in host time: a CLI
//! has no guest clock, and against a stopped or frozen machine no
//! VMM exists to ask, thus the audit stream carries the host clock
//! alone. Machine-scoped verbs append to machines/<vm>/audit; the
//! placeless verbs (list, doctor, build, setup) to the audit file
//! at the CELLA_HOME root. The books ride the tree: branch and
//! archive carry them like every other byte.

use std::path::PathBuf;

use crate::{ledger, proto};

/// The home, resolved the way every persona resolves it -- kept
/// local so the audit feature pulls no registry code: the gateway
/// carries no machine module (1.6.13).
fn home() -> PathBuf {
    if let Ok(h) = std::env::var("CELLA_HOME") {
        return PathBuf::from(h);
    }
    let base = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join(".cella")
}

/// The book a verb lands in. A verb that names a machine whose
/// manifest exists writes that machine's book; everything else --
/// the placeless verbs, a create whose machine is not born yet, a
/// look at a name that never existed -- writes the root book. The
/// rule is uniform, and it leaves no ghost directories: the book
/// never creates a machine.
fn book_path(vm: Option<&str>) -> PathBuf {
    if let Some(vm) = vm {
        let dir = home().join("machines").join(vm);
        if dir.join("manifest.json").is_file() {
            return dir.join("audit");
        }
    }
    home().join("audit")
}

/// The persona: the basename this binary was invoked as, the
/// -debug suffix stripped (the same rule the dispatcher applies).
fn persona() -> String {
    let name = std::env::args()
        .next()
        .as_deref()
        .map(std::path::Path::new)
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "cella".to_string());
    name.strip_suffix("-debug")
        .map(String::from)
        .unwrap_or(name)
}

/// Witness one verb: one framed Audit entry in the right book,
/// before the verb runs. A verb that only reads still writes its
/// entry -- the pump's five shows a second make a thick file, and
/// that is the truth of what the pump does. A failure to witness
/// fails the verb: an unwitnessed action must not proceed.
pub fn witness(vm: Option<&str>, verb: &str, args: &[String]) -> Result<(), String> {
    // SAFETY: getuid and getgid read facts and cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let owned_verb = verb.to_string();
    let args = args.to_vec();
    let persona = persona();
    let host_ns = ledger::host_ns_now();
    ledger::append_chained(&book_path(vm), |predecessor| proto::Message {
        body: Some(proto::message::Body::Audit(proto::Audit {
            verb: owned_verb,
            args,
            uid,
            gid,
            persona,
            host_ns,
            predecessor,
        })),
    })
    .map_err(|e| format!("witnessing {verb:?} failed -- the verb does not proceed: {e}"))
}
