//! The bwrap jail, generic across personas (1.6.14a). One profile
//! file per persona (security/profiles/<cli>/bwrap.txt) names its
//! bind set and its namespace set, as data; this module reads the
//! file and builds the bwrap invocation. No persona-specific code
//! lives here: a new persona gets a new profile file, not a new
//! code path.
//!
//! Two consumers:
//! - `confine_self`: a persona re-execs itself inside its own jail,
//!   near the top of main, so that even a verb that runs for
//!   milliseconds runs confined. A marker environment variable
//!   stops the jailed child from jailing itself again.
//! - `spawn_args`: the VMM's jail (machine.rs::spawn) already builds
//!   its own bwrap invocation, one level down and with dynamic,
//!   per-machine paths (the state dir, the kernel, the tap); it
//!   reads the profile for its *static* bind set instead of
//!   hard-coding it, so the profile file stays the one source of
//!   truth for both consumers.

use std::fs;
use std::path::{Path, PathBuf};

/// The marker that says "already inside the jail this profile
/// describes" -- set by the parent before the bwrap exec, read by
/// the child so it does not jail itself a second time.
pub const JAILED_ENV: &str = "CELLA_JAILED";

/// One profile: the namespace set and the bind set, exactly as the
/// file names them.
#[derive(Debug, Default, Clone)]
pub struct Profile {
    pub unshare_user: bool,
    pub unshare_pid: bool,
    pub unshare_ipc: bool,
    pub unshare_uts: bool,
    pub unshare_cgroup: bool,
    pub unshare_net: bool,
    pub proc: bool,
    /// bwrap --dev /dev: the minimal device set (null, zero, random,
    /// tty and friends). A persona that spawns a child with a null
    /// stdio needs /dev/null to exist; the kvm node is a dev-bind on
    /// top of it.
    pub dev: bool,
    pub tmp: bool,
    pub new_session: bool,
    /// Literal host paths, read-only.
    pub ro_binds: Vec<String>,
    /// Literal host paths, read-write.
    pub rw_binds: Vec<String>,
    /// Device paths (--dev-bind).
    pub dev_binds: Vec<String>,
}

/// The profile directory root: CELLA_PROFILES_DIR overrides (an
/// install lays profiles wherever it lays the rest of the tree);
/// otherwise the compiled-in path back to security/profiles/, which
/// resolves for every dev and smoke build (cargo always compiles
/// this crate from the same checkout it ships in).
pub fn profiles_root() -> PathBuf {
    if let Ok(p) = std::env::var("CELLA_PROFILES_DIR") {
        return PathBuf::from(p);
    }
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../security/profiles"
    ))
}

/// The profile text compiled into the binary, one per persona. The
/// file under security/profiles/ is the source of truth at run time
/// when it exists; this copy serves a binary that runs where no
/// checkout exists -- the static cella inside a nested guest, or an
/// install without the repository -- and it is the same bytes.
fn embedded(persona: &str) -> Option<&'static str> {
    Some(match persona {
        "cella" => include_str!("../../../security/profiles/cella/bwrap.txt"),
        "cella-build" => include_str!("../../../security/profiles/cella-build/bwrap.txt"),
        "cella-doctor" => include_str!("../../../security/profiles/cella-doctor/bwrap.txt"),
        "cella-gateway" => include_str!("../../../security/profiles/cella-gateway/bwrap.txt"),
        "cella-machine" => include_str!("../../../security/profiles/cella-machine/bwrap.txt"),
        "cella-network" => include_str!("../../../security/profiles/cella-network/bwrap.txt"),
        "cella-probe" => include_str!("../../../security/profiles/cella-probe/bwrap.txt"),
        "cella-universe" => include_str!("../../../security/profiles/cella-universe/bwrap.txt"),
        "cella-vmm" => include_str!("../../../security/profiles/cella-vmm/bwrap.txt"),
        _ => return None,
    })
}

/// Parse one profile file. A line is a directive and its argument,
/// or a comment (#) or blank line. An unknown directive is an
/// error: a profile that cannot be understood must not be silently
/// narrowed or widened.
pub fn load(persona: &str) -> Result<Profile, String> {
    let path = profiles_root().join(persona).join("bwrap.txt");
    let text = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => match embedded(persona) {
            Some(t) => t.to_string(),
            None => {
                return Err(format!(
                    "reading the {persona} jail profile ({}): {e}",
                    path.display()
                ))
            }
        },
    };
    let mut p = Profile::default();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.splitn(2, char::is_whitespace);
        let directive = it.next().unwrap_or("");
        let arg = it.next().unwrap_or("").trim();
        match directive {
            "unshare-user" => p.unshare_user = true,
            "unshare-pid" => p.unshare_pid = true,
            "unshare-ipc" => p.unshare_ipc = true,
            "unshare-uts" => p.unshare_uts = true,
            "unshare-cgroup" => p.unshare_cgroup = true,
            "unshare-net" => p.unshare_net = true,
            "proc" => p.proc = true,
            "dev" => p.dev = true,
            "tmpfs-tmp" => p.tmp = true,
            "new-session" => p.new_session = true,
            "ro-bind" if !arg.is_empty() => p.ro_binds.push(arg.to_string()),
            "bind" if !arg.is_empty() => p.rw_binds.push(arg.to_string()),
            "dev-bind" if !arg.is_empty() => p.dev_binds.push(arg.to_string()),
            _ => {
                return Err(format!(
                    "{}:{}: unknown or malformed directive {line:?}",
                    path.display(),
                    n + 1
                ))
            }
        }
    }
    Ok(p)
}

/// Append this profile's namespace flags and static binds to a
/// bwrap command line, skipping any bind whose host path does not
/// exist (a profile names the full bind set across every host this
/// persona might run on; a busybox guest has no /lib64).
pub fn apply(profile: &Profile, args: &mut Vec<String>) {
    let flag = |args: &mut Vec<String>, on: bool, name: &str| {
        if on {
            args.push(name.to_string());
        }
    };
    flag(args, profile.unshare_user, "--unshare-user");
    flag(args, profile.unshare_pid, "--unshare-pid");
    flag(args, profile.unshare_ipc, "--unshare-ipc");
    flag(args, profile.unshare_uts, "--unshare-uts");
    flag(args, profile.unshare_cgroup, "--unshare-cgroup");
    flag(args, profile.unshare_net, "--unshare-net");
    for p in &profile.ro_binds {
        if Path::new(p).exists() {
            args.push("--ro-bind".into());
            args.push(p.clone());
            args.push(p.clone());
        }
    }
    for p in &profile.rw_binds {
        if Path::new(p).exists() {
            args.push("--bind".into());
            args.push(p.clone());
            args.push(p.clone());
        }
    }
    // --dev before any --dev-bind: the node binds mount on top of
    // the minimal /dev, not the other way round.
    if profile.dev {
        args.push("--dev".into());
        args.push("/dev".into());
    }
    for p in &profile.dev_binds {
        if Path::new(p).exists() {
            args.push("--dev-bind".into());
            args.push(p.clone());
            args.push(p.clone());
        }
    }
    if profile.proc {
        args.push("--proc".into());
        args.push("/proc".into());
    }
    if profile.tmp {
        args.push("--tmpfs".into());
        args.push("/tmp".into());
    }
    if profile.new_session {
        args.push("--new-session".into());
    }
}

/// Re-exec the current process inside its own jail, described by
/// security/profiles/<persona>/bwrap.txt. A no-op when the marker
/// says the process is already jailed. There is no escape hatch: a
/// persona that cannot jail does not run. Never returns on the
/// confining path: exec replaces this
/// process with bwrap, which in turn execs the same binary again
/// with the marker set, and that second incarnation returns here to
/// find the marker and fall through to real work.
///
/// `extra_binds` names paths this invocation needs beyond the
/// profile's static set (a machine name resolved to its directory,
/// for instance), read-write. `extra_ro_binds` names the paths it
/// only reads (the goldens, a sibling binary's directory). Both are
/// resolved at run time because they move per install and per test
/// sandbox; a profile names literal host paths alone.
pub fn confine_self(
    persona: &str,
    extra_binds: &[PathBuf],
    extra_ro_binds: &[PathBuf],
) -> Result<(), String> {
    if std::env::var(JAILED_ENV).is_ok() {
        return Ok(());
    }
    let profile = load(persona)?;
    let me = std::env::current_exe().map_err(|e| format!("finding this binary: {e}"))?;
    let bwrap = find_program("bwrap");
    let mut args: Vec<String> = Vec::new();
    apply(&profile, &mut args);
    for p in extra_ro_binds {
        if p.exists() {
            let s = p.to_string_lossy().to_string();
            args.push("--ro-bind".into());
            args.push(s.clone());
            args.push(s);
        }
    }
    for p in extra_binds {
        if p.exists() {
            let s = p.to_string_lossy().to_string();
            args.push("--bind".into());
            args.push(s.clone());
            args.push(s);
        }
    }
    // The binary itself, read-only, so that the re-exec inside the
    // jail finds the same inode the parent ran.
    if let Some(dir) = me.parent() {
        let s = dir.to_string_lossy().to_string();
        args.push("--ro-bind".into());
        args.push(s.clone());
        args.push(s);
    }
    args.push(me.to_string_lossy().to_string());
    args.extend(std::env::args().skip(1));
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(&bwrap)
        .args(&args)
        .env(JAILED_ENV, "1")
        .exec();
    Err(format!("exec {bwrap}: {err}"))
}

/// Resolve an external program to an absolute path: the PATH of a
/// re-exec'd or namespaced process is not a given (see machine.rs).
pub fn find_program(name: &str) -> String {
    for dir in ["/usr/bin", "/bin", "/usr/sbin", "/sbin", "/usr/local/bin"] {
        let p = format!("{dir}/{name}");
        if Path::new(&p).is_file() {
            return p;
        }
    }
    name.to_string()
}
