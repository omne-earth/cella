//! The machine registry: create, destroy, and the golden-artifact
//! paths. See docs/LIFECYCLE.md.
//!
//! Daemonless: a directory under machines/ is a machine, and its
//! manifest.json is the record. Every write goes to a temporary file
//! and then renames, the same crash rule as the freeze sidecar. No
//! global state exists: a verb's transaction is one directory.
//!
//! The manifest is a flat JSON object with string and number values.
//! The parser and the writer live here, hand-rolled: the repository
//! takes no serialization dependency for one flat object (the same
//! reasoning as the hand-rolled BPF in seccomp.rs).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The operational home. CELLA_HOME overrides it, for the tests and
/// for a relocated installation.
pub fn home() -> PathBuf {
    if let Ok(h) = std::env::var("CELLA_HOME") {
        return PathBuf::from(h);
    }
    let base = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join(".cella")
}

pub fn kernel_path(flavor: &str) -> PathBuf {
    home().join("kernel").join(flavor).join("bzImage")
}

pub fn rootfs_path(flavor: &str) -> PathBuf {
    home().join("rootfs").join(flavor).join("rootfs.ext4")
}

pub fn machine_dir(name: &str) -> PathBuf {
    home().join("machines").join(name)
}

/// A machine name is a path component. Restrict it, so that a name
/// cannot escape the machines directory or confuse a shell.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
}

/// The fixed configuration of a machine. `create` writes it once;
/// `start` reads it and takes no flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub kernel: String,
    pub rootfs: String,
    pub mem_mb: u64,
    /// The nic list ("none", "world[:PORTS]", "wire:NAME", comma-joined).
    pub net: String,
    /// "rw" or "ro".
    pub root: String,
    /// "on" adds cella_diag to the kernel command line: the image
    /// then prints its heartbeat and its diagnostic listings.
    pub diag: String,
    /// A path to a disk attached read-only as a second virtio-blk
    /// (the rock of the inspect verb), or "none".
    pub attach: String,
}

/// Read one field of a flat JSON object: a quoted string or a bare
/// number. Returns None when the key is absent.
/// One field of a raw manifest text, for the readers outside this
/// module (doctor, the universe family).
pub fn manifest_field(s: &str, key: &str) -> Option<String> {
    json_field(s, key).map(str::to_string)
}

fn json_field<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let i = s.find(&pat)?;
    let rest = s[i + pat.len()..].trim_start();
    if let Some(r) = rest.strip_prefix('"') {
        r.split('"').next()
    } else {
        rest.split(|c: char| c == ',' || c == '}' || c.is_whitespace())
            .next()
    }
}

/// The defaults of create, from ~/.cella/config.json. Every field is
/// optional; an absent file or an absent field falls back to the
/// built-in default. Flags override both.
pub fn defaults() -> Manifest {
    let mut m = Manifest {
        name: String::new(),
        kernel: "canonical".into(),
        rootfs: "cella".into(),
        mem_mb: 256,
        net: "none".into(),
        root: "rw".into(),
        diag: "off".into(),
        attach: "none".into(),
    };
    let path = home().join("config.json");
    let Ok(s) = fs::read_to_string(&path) else {
        // Seed the file with the built-ins on first use, so that the
        // knobs are discoverable by reading it. A seeded file changes
        // nothing: its values are the built-ins.
        let seed = format!(
            "{{\n  \"kernel\": \"{}\",\n  \"rootfs\": \"{}\",\n  \"mem_mb\": {},\n  \"net\": \"{}\",\n  \"root\": \"{}\",\n  \"diag\": \"{}\"\n}}\n",
            m.kernel, m.rootfs, m.mem_mb, m.net, m.root, m.diag
        );
        if fs::create_dir_all(home()).is_ok() {
            let _ = write_atomic(&path, seed.as_bytes());
        }
        return m;
    };
    if let Some(v) = json_field(&s, "kernel") {
        m.kernel = v.to_string();
    }
    if let Some(v) = json_field(&s, "rootfs") {
        m.rootfs = v.to_string();
    }
    if let Some(v) = json_field(&s, "mem_mb").and_then(|v| v.parse().ok()) {
        m.mem_mb = v;
    }
    if let Some(v) = json_field(&s, "net") {
        m.net = v.to_string();
    }
    if let Some(v) = json_field(&s, "root") {
        m.root = v.to_string();
    }
    if let Some(v) = json_field(&s, "diag") {
        m.diag = v.to_string();
    }
    m
}

impl Manifest {
    pub fn to_json(&self) -> String {
        format!(
            "{{\n  \"name\": \"{}\",\n  \"kernel\": \"{}\",\n  \"rootfs\": \"{}\",\n  \"mem_mb\": {},\n  \"net\": \"{}\",\n  \"root\": \"{}\",\n  \"diag\": \"{}\",\n  \"attach\": \"{}\"\n}}\n",
            self.name, self.kernel, self.rootfs, self.mem_mb, self.net, self.root, self.diag, self.attach
        )
    }

    /// Parse the flat object. The fields are validated names and
    /// numbers, thus no escape handling is necessary; a manifest that
    /// does not parse is an error, not a guess.
    pub fn from_json(s: &str) -> Result<Manifest, String> {
        let field = |s: &str, key: &str| -> Result<String, String> {
            json_field(s, key)
                .map(str::to_string)
                .ok_or_else(|| format!("missing field {key}"))
        };
        Ok(Manifest {
            name: field(s, "name")?.to_string(),
            kernel: field(s, "kernel")?.to_string(),
            rootfs: field(s, "rootfs")?.to_string(),
            mem_mb: field(s, "mem_mb")?
                .parse()
                .map_err(|_| "mem_mb is not a number".to_string())?,
            net: field(s, "net")?.to_string(),
            root: field(s, "root")?.to_string(),
            // Absent in older manifests: default off.
            diag: json_field(s, "diag").unwrap_or("off").to_string(),
            attach: json_field(s, "attach").unwrap_or("none").to_string(),
        })
    }
}

/// Write a file with the crash rule of the sidecar: temporary file,
/// fsync, rename, fsync of the directory.
fn write_atomic(path: &Path, content: &[u8]) -> io::Result<()> {
    let dir = path.parent().expect("path has a parent");
    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(content)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)?;
    let d = fs::File::open(dir)?;
    // SAFETY: d is an open fd for the duration of the call.
    unsafe {
        libc::fsync(std::os::fd::AsRawFd::as_raw_fd(&d));
    }
    Ok(())
}

/// Update one field of a manifest in place, preserving every field
/// this struct does not carry (the universe digests, the latch).
/// The valve automaton's record: one word beside the machine,
/// never a manifest identity field. Two automata, two controllers
/// (docs/FREEZE-THAW.md, "The two automata"): the machine verbs
/// own running and frozen, the gateway verbs own closed and open,
/// and neither writes the other's state. create writes the birth
/// state once; only the gateway verbs call the setter after it.
pub fn valve_record(name: &str) -> String {
    fs::read_to_string(machine_dir(name).join("valve"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "closed".to_string())
}

pub fn set_valve_record(name: &str, word: &str) -> Result<(), String> {
    let p = machine_dir(name).join("valve");
    write_atomic(&p, format!("{word}\n").as_bytes())
        .map_err(|e| format!("writing the valve record: {e}"))
}

pub fn read_manifest(name: &str) -> Result<Manifest, String> {
    let p = machine_dir(name).join("manifest.json");
    let s = fs::read_to_string(&p).map_err(|e| format!("reading {}: {e}", p.display()))?;
    Manifest::from_json(&s)
}

/// The privileged setup: provision a pool of persistent taps and the
/// NAT, once. This is the one verb that needs root; everything else
/// runs rootless, and create allocates from this pool (--net auto).
/// Resolve an external program to an absolute path. The PATH search
/// of execvp is not dependable from a static binary inside a guest
/// (observed: ENOENT for a program that the shell finds), thus every
/// spawn resolves the path itself.
fn find_program(name: &str) -> String {
    for dir in ["/usr/bin", "/bin", "/usr/sbin", "/sbin", "/usr/local/bin"] {
        let p = format!("{dir}/{name}");
        if Path::new(&p).is_file() {
            return p;
        }
    }
    name.to_string()
}

/// The translator's lifecycle, spawn side (1.6.14e rung 2): a
/// machine with wire nics needs its translator standing before
/// the VMM connects. Machine-lifetime: spawned here on the first
/// start (or after a translator crash), killed by destroy. The
/// binary resolves like every sibling: beside the current
/// binary first (the lab runs the lab's), then the install.
fn ensure_translator(name: &str, dir: &Path) -> Result<(), String> {
    if let Ok(pid) = fs::read_to_string(dir.join("edge.pid")) {
        if let Ok(pid) = pid.trim().parse::<i32>() {
            // SAFETY: signal 0 probes liveness only.
            if unsafe { libc::kill(pid, 0) } == 0 {
                return Ok(());
            }
        }
    }
    let bin = std::env::current_exe()
        .ok()
        .and_then(|me| {
            let p = me.parent()?.join("cella-network");
            p.is_file().then_some(p)
        })
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| find_program("cella-network"));
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("edge.log"))
        .map_err(|e| format!("opening edge.log: {e}"))?;
    let log2 = log
        .try_clone()
        .map_err(|e| format!("cloning edge.log: {e}"))?;
    use std::os::unix::process::CommandExt;
    let mut c = std::process::Command::new(&bin);
    c.args(["edge", name])
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log2);
    // SAFETY: setsid detaches the translator from this verb's
    // session; nothing async-signal-unsafe runs in the hook.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn()
        .map_err(|e| format!("spawning the translator for {name:?}: {e}"))?;
    Ok(())
}

/// The wire plane's spawn side (1.6.14e rung 2): connect one nic
/// to this machine's translator on edge.sock, send the one-byte
/// hello naming the nic index, and hand back an inheritable fd
/// for --edge-fd. Retries until the translator stands, inside
/// `patience` -- start spawns the translator and connects in the
/// same breath, and the translator's listen may land second.
pub fn connect_edge_nic(
    dir: &Path,
    nic_index: u8,
    patience: std::time::Duration,
) -> Result<i32, String> {
    let path = dir.join("edge.sock");
    let deadline = std::time::Instant::now() + patience;
    loop {
        match crate::seq::connect(&path) {
            Ok(fd) => {
                // SAFETY: one byte from a valid buffer to a live fd.
                let n = unsafe { libc::write(fd, [nic_index].as_ptr() as *const libc::c_void, 1) };
                if n != 1 {
                    // SAFETY: fd is ours.
                    unsafe { libc::close(fd) };
                    return Err(format!("hello to {path:?} failed"));
                }
                crate::seq::inheritable(fd);
                return Ok(fd);
            }
            Err(e) => {
                if std::time::Instant::now() >= deadline {
                    return Err(format!("the translator did not answer on {path:?}: {e}"));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

/// Stage a machine: verify the goldens, resolve the network claim,
/// copy the rootfs flavor to the machine's own disk, and write the
/// manifest. No process starts. The manifest records the resolved tap
/// name, thus two machines cannot share one tap.
pub fn create(m: &Manifest) -> Result<(), String> {
    if !valid_name(&m.name) {
        return Err(format!(
            "invalid machine name {:?}: lowercase letters, digits, and dashes",
            m.name
        ));
    }
    let dir = machine_dir(&m.name);
    if dir.exists() {
        return Err(format!(
            "machine {:?} already exists -- destroy it first, or pick another name",
            m.name
        ));
    }
    let m = m.clone();
    if m.net != "none" {
        // Every nic is the translator's (1.6.14e): a wire or the
        // world. There is no host object to claim, and no "auto".
        for nic in m.net.split(',') {
            let nic = nic.trim();
            if let Some(spec) = nic.strip_prefix("world:") {
                for item in spec.split('+').filter(|s| !s.is_empty()) {
                    let ok = item
                        .split_once('/')
                        .map(|(p, proto)| {
                            p.parse::<u16>().is_ok() && (proto == "tcp" || proto == "udp")
                        })
                        .unwrap_or(false);
                    if !ok {
                        return Err(format!(
                            "port map {item:?} in {nic:?} -- want PORT/tcp or PORT/udp, \
                             joined by '+'"
                        ));
                    }
                }
            } else if let Some(w) = nic.strip_prefix("wire:") {
                let ok = !w.is_empty()
                    && w.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
                if !ok {
                    return Err(format!(
                        "wire name {w:?} -- lowercase letters, digits, and '-' only"
                    ));
                }
            } else if nic != "world" {
                return Err(format!(
                    "nic {nic:?} -- want world, world:PORT/tcp+..., wire:NAME, or none"
                ));
            }
        }
    }
    let kernel = kernel_path(&m.kernel);
    let rootfs = rootfs_path(&m.rootfs);
    for (what, p, flavor) in [
        ("kernel", &kernel, &m.kernel),
        ("rootfs", &rootfs, &m.rootfs),
    ] {
        if !p.is_file() {
            return Err(format!(
                "golden {what} flavor {flavor:?} missing at {} -- run: cella build {what} {flavor}",
                p.display()
            ));
        }
    }
    fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    fs::copy(&rootfs, dir.join("disk.img")).map_err(|e| format!("copying the disk: {e}"))?;
    write_atomic(&dir.join("manifest.json"), m.to_json().as_bytes())
        .map_err(|e| format!("writing the manifest: {e}"))?;
    // The valve automaton's birth state: closed. The record lives
    // beside the machine, and only the gateway verbs change it.
    set_valve_record(&m.name, "closed")?;
    Ok(())
}

/// True when the machine's pid file names a live process.
pub fn is_running(name: &str) -> bool {
    let p = machine_dir(name).join("pid");
    let Ok(s) = fs::read_to_string(&p) else {
        return false;
    };
    let Ok(pid) = s.trim().parse::<i32>() else {
        return false;
    };
    // SAFETY: signal 0 probes for existence and sends nothing.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Delete the machine, once and for all. Refuses a running machine.
pub fn destroy(name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("invalid machine name {name:?}"));
    }
    let dir = machine_dir(name);
    if !dir.exists() {
        return Err(format!("no machine named {name:?}"));
    }
    if is_running(name) {
        return Err(format!("machine {name:?} is running -- stop it first"));
    }
    // The translator dies with its machine (1.6.14e): destroy is
    // the end of the machine's lifetime, and the translator's.
    if let Ok(pid) = fs::read_to_string(dir.join("edge.pid")) {
        if let Ok(pid) = pid.trim().parse::<i32>() {
            // SAFETY: pid comes from this machine's own edge.pid.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
    fs::remove_dir_all(&dir).map_err(|e| format!("removing {}: {e}", dir.display()))
}

/// The build verb: every golden artifact builds natively (see
/// src/build.rs and docs/LIFECYCLE.md). The flavor decides the
// --- start and stop ---------------------------------------------------
use crate::config;

fn pid_path(name: &str) -> PathBuf {
    machine_dir(name).join("pid")
}

/// The transients of a machine: everything that only a running or a
/// crashed machine leaves behind. A stopped machine is its manifest
/// and its disk, nothing else.
fn clear_transients(name: &str) -> Vec<&'static str> {
    let dir = machine_dir(name);
    let mut cleared = Vec::new();
    for (file, label) in [
        ("ram.img", "ram.img"),
        ("pid", "pid"),
        ("console.sock", "console.sock"),
        ("state.tmp", "state.tmp"),
    ] {
        if fs::remove_file(dir.join(file)).is_ok() {
            cleared.push(label);
        }
    }
    cleared
}

pub fn is_frozen(name: &str) -> bool {
    machine_dir(name).join("state").is_file()
}

/// The kernel command line of a machine, from its manifest. One
/// source: config.rs holds the defaults, and the manifest holds the
/// choices of create.
fn cmdline_for(m: &Manifest) -> String {
    let mut base = config::default_cmdline();
    if m.diag == "on" {
        base.push_str(" cella_diag");
    }
    if m.attach != "none" {
        // The attached rock is the second virtio-blk (/dev/vdb),
        // read-only at the device; the inspector runs airgapped.
        return format!(
            "{base} root=/dev/vda {} virtio_mmio.device=4K@0xd0000000:5 \
             virtio_mmio.device=4K@0xd0002000:7",
            m.root
        );
    }
    if m.net == "none" {
        return format!(
            "{base} root=/dev/vda {} virtio_mmio.device=4K@0xd0000000:5",
            m.root
        );
    }
    // One device entry per nic: eth<i> at base + i*0x2000, IRQ
    // 6 + 2*i (the attach slot between them never moves). The ip=
    // parameter configures eth0 only; the init of a multi-net image
    // (the gateway flavor) configures the rest.
    let nics: Vec<&str> = m.net.split(',').map(str::trim).collect();
    let mut line = format!(
        "{base} root=/dev/vda {} virtio_mmio.device=4K@0xd0000000:5",
        m.root
    );
    for (i, _) in nics.iter().enumerate() {
        line.push_str(&format!(
            " virtio_mmio.device=4K@{:#x}:{}",
            0xd000_1000u64 + (i as u64) * 0x2000,
            6 + 2 * i
        ));
    }
    // The first nic configures eth0. A world nic gets the
    // translator's convention (guest .2, gateway .1); a wire nic
    // gets no ip= -- the guests address the wire themselves.
    let first = nics[0];
    if first.starts_with("world") {
        // The world nic's contract (1.6.14e rung 3): the pool
        // convention on the translator's own subnet -- see
        // config::WORLD_GUEST_IP.
        let g = config::WORLD_GUEST_IP;
        let w = config::WORLD_GW_IP;
        line.push_str(&format!(
            " ip={}.{}.{}.{}::{}.{}.{}.{}:255.255.255.0::eth0:off",
            g[0], g[1], g[2], g[3], w[0], w[1], w[2], w[3]
        ));
        return line;
    }
    // A wire nic carries no host addressing convention: the guest
    // (or the gate driving its console) configures eth0 itself.
    line
}

/// Start the machine: a fresh boot. Refuses a frozen machine, so
/// that a sidecar cannot vanish by accident; thaw is the verb for it.
/// A rock: the manifest latched state=archived (the archive verb).
/// The latch reads from the raw text, thus a manifest with fields
/// this struct does not carry still latches.
pub fn is_archived(name: &str) -> bool {
    fs::read_to_string(machine_dir(name).join("manifest.json"))
        .ok()
        .and_then(|s| json_field(&s, "state").map(|v| v == "archived"))
        .unwrap_or(false)
}

fn refuse_rock(name: &str, verb: &str) -> Result<(), String> {
    if is_archived(name) {
        return Err(format!(
            "machine {name:?} is archived (a rock) -- {verb} refuses it; inspect is the verb for a rock"
        ));
    }
    Ok(())
}

pub fn start(name: &str) -> Result<(), String> {
    refuse_rock(name, "start")?;
    if is_frozen(name) {
        return Err(format!("machine {name:?} is frozen -- thaw it"));
    }
    spawn(name, "started")
}

/// Thaw the frozen machine: the same spawn, and the VMM detects the
/// sidecar and resumes instead of booting. Readiness fires after the
/// warming and the clock restore, thus the verb returns when the
/// guest lives again on its frozen clock.
pub fn thaw(name: &str) -> Result<(), String> {
    refuse_rock(name, "thaw")?;
    if !is_frozen(name) {
        return Err(format!(
            "machine {name:?} is not frozen -- start it, or freeze it first"
        ));
    }
    spawn(name, "thawed")
}

/// Freeze the running machine: SIGUSR1 to the VMM, then wait for the
/// process to exit and for the sidecar to appear. The pid file holds
/// the host pid of the VMM itself (bwrap reports it at spawn through
/// --info-fd), thus the signal goes straight to the right process.
pub fn freeze(name: &str) -> Result<(), String> {
    if !is_running(name) {
        return Err(format!("machine {name:?} is not running"));
    }
    let pid: i32 = fs::read_to_string(pid_path(name))
        .map_err(|e| format!("reading the pid file: {e}"))?
        .trim()
        .parse()
        .map_err(|_| "the pid file is not a number".to_string())?;
    // SAFETY: pid comes from the machine's own pid file.
    unsafe { libc::kill(pid, libc::SIGUSR1) };
    for _ in 0..600 {
        // SAFETY: signal 0 probes only.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if !alive && is_frozen(name) {
            let _ = fs::remove_file(pid_path(name));
            println!("cella: machine {name:?} frozen");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err(format!(
        "machine {name:?} did not freeze within the timeout -- see {}",
        machine_dir(name).join("vmm.log").display()
    ))
}

/// The delegated sub-id range for the invoking user: one line of
/// /etc/subuid or /etc/subgid, "<user>:<base>:<count>". This is the
/// host prerequisite for the identity mapping below -- an
/// administrator runs `usermod --add-subuids` (or edits the file
/// directly) once, out of band, the same one-time spending as
/// cella-network's file capability.
fn subid_range(file: &str) -> Result<(u32, u32), String> {
    let user = std::env::var("USER").unwrap_or_else(|_| {
        std::process::Command::new("id")
            .arg("-un")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    });
    let text = fs::read_to_string(file).map_err(|e| format!("reading {file}: {e}"))?;
    text.lines()
        .find_map(|l| {
            let mut f = l.splitn(3, ':');
            let u = f.next()?;
            if u != user {
                return None;
            }
            let base: u32 = f.next()?.parse().ok()?;
            let count: u32 = f.next()?.parse().ok()?;
            Some((base, count))
        })
        .ok_or_else(|| {
            format!(
                "no {file} entry for user {user:?} -- the identity mapping needs a delegated \
                 sub-id range (see docs/LIFECYCLE.md, \"The security boundary\"): \
                 sudo usermod --add-subuids {r} --add-subgids {r} {user}",
                r = crate::config::SUBID_RANGE_HINT
            )
        })
}

/// The host uid/gid this machine runs as, mapped by the spawn: a
/// distinct sub-user per machine (the lane's standing ruling), never
/// the invoking user's own uid. The offset into the delegated range
/// is allocated once, at the machine's first spawn, and persisted in
/// its directory (dir/uid) so that a thaw after a freeze keeps the
/// same identity -- the same allocation pattern as the tap pool
/// (allocate_tap/claimed_taps): first free slot, scanned across the
/// sibling machine directories, never reused while another machine
/// still claims it.
fn machine_identity(name: &str, dir: &Path) -> Result<(u32, u32), String> {
    let uid_path = dir.join("uid");
    if let Ok(s) = fs::read_to_string(&uid_path) {
        if let Ok(off) = s.trim().parse::<u32>() {
            let (ubase, _) = subid_range("/etc/subuid")?;
            let (gbase, _) = subid_range("/etc/subgid")?;
            return Ok((ubase + off, gbase + off));
        }
    }
    let (ubase, ucount) = subid_range("/etc/subuid")?;
    let (gbase, gcount) = subid_range("/etc/subgid")?;
    let count = ucount.min(gcount);
    let claimed: Vec<u32> = fs::read_dir(home().join("machines"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| fs::read_to_string(e.path().join("uid")).ok())
                .filter_map(|s| s.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default();
    let offset = (0..count)
        .find(|o| !claimed.contains(o))
        .ok_or_else(|| format!("no free sub-id slot in the delegated range ({count} wide)"))?;
    write_atomic(&uid_path, format!("{offset}\n").as_bytes())
        .map_err(|e| format!("writing {name}'s identity: {e}"))?;
    Ok((ubase + offset, gbase + offset))
}

/// The shared spawn of start and thaw: the VMM inside the jail,
/// detached, with readiness on a pipe. The jail's static bind set
/// and namespace set come from security/profiles/cella-vmm/bwrap.txt
/// (cella_libs::jail); this function adds the dynamic, per-machine
/// paths and the identity mapping, then invokes bwrap directly, with
/// no shell. One deliberate difference from a plain jail: no
/// --die-with-parent. The verb process exits after readiness, and
/// the machine must survive it; the pid file and stop own the
/// cleanup instead.
fn spawn(name: &str, done_word: &str) -> Result<(), String> {
    let m = read_manifest(name)?;
    if is_running(name) {
        return Err(format!("machine {name:?} is already running"));
    }
    // A thaw must keep ram.img and the sidecar: they are the frozen
    // guest. A fresh start clears the leftovers of a crash.
    if !is_frozen(name) {
        clear_transients(name);
    }
    let dir = machine_dir(name);
    let kernel = kernel_path(&m.kernel);
    if !kernel.is_file() {
        return Err(format!(
            "golden kernel {:?} missing -- run: cella build kernel {}",
            m.kernel, m.kernel
        ));
    }
    // The VMM is its own binary since the split (1.6.13): resolve
    // the sibling beside the invoking persona's inode, same flavor
    // -- -debug pairs with -debug, and the field never runs a lab
    // VMM. The jail still binds it as /cella-vmm.
    let me = std::env::current_exe().map_err(|e| format!("finding the binary: {e}"))?;
    let me_dir = me
        .parent()
        .ok_or_else(|| "the binary has no parent directory".to_string())?;
    let lab = me
        .file_name()
        .map(|n| n.to_string_lossy().ends_with("-debug"))
        .unwrap_or(false);
    let bin = me_dir.join(if lab { "cella-vmm-debug" } else { "cella-vmm" });
    if !bin.is_file() {
        return Err(format!(
            "the VMM binary is missing beside this one: {}",
            bin.display()
        ));
    }
    let kernel_dir = kernel.parent().expect("kernel path has a parent");

    // The readiness pipe. The child inherits the write end, and the
    // VMM writes one line on it immediately before the first KVM_RUN
    // (see CELLA_READY_FD in main.rs). CLOEXEC, deliberately: any
    // OTHER child this function spawns (the translator, 1.6.14e)
    // must not inherit a write end, or the parent's read below
    // never sees EOF and start hangs forever; the raw-fork child
    // clears the flag on exactly its own two fds before execv.
    let mut fds = [0i32; 2];
    // SAFETY: fds is a valid two-element array.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err("creating the readiness pipe failed".to_string());
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    // The info pipe. bwrap reports the host pid of its child (the VMM)
    // on --info-fd, and that pid is the fact the pid file records: the
    // freeze signal then goes straight to the VMM, with no process
    // walking. CLOEXEC for the same reason as the readiness pipe.
    let mut ifds = [0i32; 2];
    // SAFETY: ifds is a valid two-element array.
    if unsafe { libc::pipe2(ifds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err("creating the info pipe failed".to_string());
    }
    let (info_read, info_write) = (ifds[0], ifds[1]);

    // The console exists only in the lab (debug-assertions on). A
    // release machine gets no console.log and no console.sock: its
    // ttyS0 has no listener, and the VMM discards the bytes.
    // A plain File, not a Stdio: the identity mapping below forks and
    // execs by hand (Command::spawn's own pre_exec cannot block on a
    // handshake with its own caller without deadlocking spawn()
    // itself -- see the comment above the fork), and a raw fork needs
    // a raw fd to dup2, not the opaque Stdio enum.
    let console: fs::File = if cfg!(debug_assertions) {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("console.log"))
            .map_err(|e| format!("opening console.log: {e}"))?
    } else {
        fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .map_err(|e| format!("opening /dev/null: {e}"))?
    };
    let vmm_log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("vmm.log"))
        .map_err(|e| format!("opening vmm.log: {e}"))?;

    // The bind set and the namespace set are data:
    // security/profiles/cella-vmm/bwrap.txt (cella_libs::jail). This
    // function adds only the dynamic, per-machine paths the profile
    // cannot name (the state dir, the kernel dir, the attached rock,
    // the VMM binary itself), and the identity mapping below.
    let profile = crate::jail::load("cella-vmm")?;

    // The identity mapping: a distinct sub-user for this machine,
    // never the invoking user's own uid (the standing ruling: no
    // shared identity anywhere). bwrap's own --unshare-user always
    // self-maps (the real uid becomes namespace uid 0, mapped back to
    // the *same* real uid on the host -- namespace virtualization,
    // not a new identity), thus it is not the mechanism here: this
    // process unshares the user namespace itself, in pre_exec, and
    // claims namespace uid/gid 0 with setresuid/setresgid while it
    // still holds the creator's grace-period capabilities (a fresh,
    // still-unmapped namespace grants its creator a full capability
    // set, before any uid_map is written). It then blocks on a pipe;
    // this function, still outside the namespace, calls newuidmap
    // and newgidmap -- the setuid-root helpers that honor the range
    // /etc/subuid and /etc/subgid delegate to this user (see
    // subid_range above) -- to map that namespace uid 0 to this
    // machine's distinct host uid. Only once that mapping lands does
    // the child resume and exec bwrap: from here on, every file the
    // VMM creates is owned, on the host, by this machine's own uid,
    // not the invoking user's.
    // A root operator (a guest's init running cella one layer down)
    // needs no delegated range and no ACL: root writes the maps
    // itself and bypasses DAC. The identity boundary is the
    // unprivileged host's; in a throwaway guest, root maps 0 -> 0.
    // SAFETY: getuid has no failure mode.
    let root = unsafe { libc::getuid() } == 0;
    let (host_uid, host_gid) = if root {
        (0, 0)
    } else {
        machine_identity(name, &dir)?
    };
    // The state directory is owned by the invoking user (create()'s
    // mkdir), and every verb process still needs to read and write
    // it as that user (the pid file, the valve record, vmm.log).
    // Chowning it to the machine's own sub-user would lock the
    // invoking user out; leaving its permission bits as they are
    // would lock the sub-user out (the VMM cannot open disk.img,
    // already created by create() as the invoking user, nor create
    // ram.img or console.sock inside a directory it does not own and
    // that grants "other" no write bit). A POSIX ACL entry for this
    // one machine's host uid, set by the owning user (always
    // permitted, no privilege needed) and applied recursively (disk.img
    // already exists by the time a machine first starts; new entries
    // this same VMM creates -- ram.img, console.sock -- are simply
    // owned by it, no ACL needed for those), grants exactly this
    // directory and its current contents to exactly this uid -- a
    // different machine's uid gets no entry here, thus the
    // cross-machine refusal still holds by uid alone. The path down
    // to the state directory (this worktree, the install prefix, or
    // wherever CELLA_HOME lives) must itself stay traversable by the
    // delegated sub-id range: an ancestor directory locked to the
    // invoking user alone (a mode-0700 $HOME, for instance) refuses
    // the sub-user before it ever reaches this ACL -- a host
    // prerequisite this function cannot satisfy from here, since the
    // ancestors are outside any one machine's ownership.
    // bwrap must also *traverse* CELLA_HOME and CELLA_HOME/machines to
    // reach this one directory (a fresh sandbox -- the test suite's
    // mktemp -d, or a from-scratch install -- is mode 0700, owner
    // only); grant execute-only there, one level at a time, so a
    // different machine's directory still refuses this uid (no entry
    // widens anything below the two ancestors this loop touches).
    for ancestor in if root {
        vec![]
    } else {
        vec![home(), home().join("machines")]
    } {
        let status = std::process::Command::new(find_program("setfacl"))
            .args(["-m", &format!("u:{host_uid}:x"), ancestor.to_str().unwrap()])
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            return Err(format!(
                "granting machine {name:?}'s sub-user traversal of {} failed ({status:?})",
                ancestor.display()
            ));
        }
    }
    // Per entry, not -R: after a freeze the directory holds files
    // the machine's own sub-uid created (ram.img, the sidecar), and
    // only a file's owner may set its ACL -- a recursive grant by
    // the invoking user dies EPERM on them at thaw. Files the
    // sub-uid owns need no grant; files the invoker owns get one.
    // SAFETY: getuid has no failure mode.
    let my_uid = unsafe { libc::getuid() };
    let mut targets = if root { vec![] } else { vec![dir.clone()] };
    if let Ok(entries) = if root {
        Err(std::io::Error::other("root: no ACLs"))
    } else {
        fs::read_dir(&dir)
    } {
        for e in entries.flatten() {
            use std::os::unix::fs::MetadataExt;
            if e.metadata().map(|md| md.uid()).ok() == Some(my_uid) {
                targets.push(e.path());
            }
        }
    }
    // The default ACL on the directory makes the grant symmetric
    // for everything born later: files the VMM creates as its
    // sub-uid (network/ledger, console.sock, the sidecar) inherit
    // an entry for the invoking user, and files the verbs create
    // inherit one for the sub-uid -- otherwise each side's
    // creations lock the other out (the ledger unreadable by the
    // gateway, the socket unconnectable, destroy unable to unlink).
    // Subdirectories inherit the default ACL itself, so network/
    // propagates it without a recursive pass.
    let default_acl = format!("d:u:{my_uid}:rwx,d:u:{host_uid}:rwx");
    for target in &targets {
        let is_dir = target.is_dir();
        let mut args = vec!["-m".to_string(), format!("u:{host_uid}:rwx")];
        if is_dir {
            args.push("-m".to_string());
            args.push(default_acl.clone());
        }
        args.push(target.to_str().unwrap().to_string());
        let status = std::process::Command::new(find_program("setfacl"))
            .args(&args)
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            return Err(format!(
                "granting machine {name:?}'s sub-user access to {} failed ({status:?}) -- is \
                 the acl package (setfacl) installed, and does the filesystem under {} support \
                 POSIX ACLs?",
                target.display(),
                dir.display()
            ));
        }
    }
    let mut rfds = [0i32; 2]; // child -> parent: "I am uid/gid 0, map me"
    let mut gfds = [0i32; 2]; // parent -> child: "mapped, proceed"
                              // SAFETY: rfds and gfds are valid two-element arrays.
    if unsafe { libc::pipe(rfds.as_mut_ptr()) } != 0
        || unsafe { libc::pipe(gfds.as_mut_ptr()) } != 0
    {
        return Err("creating the identity-mapping pipes failed".to_string());
    }
    let (ready_read, ready_write) = (rfds[0], rfds[1]);
    let (go_read, go_write) = (gfds[0], gfds[1]);

    // Resolve the jail binary ourselves: the PATH of a PID-1 child
    // inside a guest is not a given, and an absolute path makes the
    // spawn error name the real fault.
    let mut cmd = std::process::Command::new(find_program("bwrap"));
    // --unshare-user is deliberately absent from this list: the
    // pre_exec closure below unshares the user namespace itself, so
    // that this process (not bwrap after another fork) is the
    // creator holding the grace-period capabilities the identity
    // mapping needs.
    for (flag, on) in [
        ("--unshare-pid", profile.unshare_pid),
        ("--unshare-ipc", profile.unshare_ipc),
        ("--unshare-uts", profile.unshare_uts),
        ("--unshare-cgroup", profile.unshare_cgroup),
    ] {
        if on {
            cmd.arg(flag);
        }
    }
    // --as-pid-1: the VMM is pid 1 of the namespace, with no bwrap
    // init in front of it. The child-pid of --info-fd is then the
    // host pid of the VMM itself, and the freeze signal lands on the
    // right process. The VMM spawns nothing, thus pid-1 reaping
    // duties are vacuous.
    cmd.args(["--as-pid-1", "--info-fd", &info_write.to_string()]);
    // The mounts apply in order, and a later mount shadows an earlier
    // one. The /tmp tmpfs therefore comes first: a machine directory
    // under /tmp (the sandboxed tests) must bind over it, not under
    // it.
    cmd.args(["--proc", "/proc", "--tmpfs", "/tmp"]);
    let ro = |c: &mut std::process::Command, p: &str| {
        c.args(["--ro-bind", p, p]);
    };
    cmd.args(["--ro-bind", bin.to_str().unwrap(), "/cella-vmm"]);
    // Library binds only where the directories exist: the busybox
    // guest has none, and the in-guest cella is static.
    for lib in &profile.ro_binds {
        if Path::new(lib).exists() {
            ro(&mut cmd, lib);
        }
    }
    for dev in &profile.dev_binds {
        if Path::new(dev).exists() {
            cmd.args(["--dev-bind", dev, dev]);
        }
    }
    let dir_s = dir.to_str().unwrap().to_string();
    cmd.args(["--bind", &dir_s, &dir_s]);
    // The attached rock enters the jail read-only: the device is
    // read-only too, and the mount adds noexec (see the guest init).
    if m.attach != "none" {
        if let Some(parent) = Path::new(&m.attach).parent() {
            let p = parent.to_str().unwrap();
            cmd.args(["--ro-bind", p, p]);
        }
    }
    cmd.args([
        "--ro-bind",
        kernel_dir.to_str().unwrap(),
        kernel_dir.to_str().unwrap(),
    ]);
    cmd.arg("--new-session");
    cmd.arg("/cella-vmm");
    cmd.args(["--state-dir", &dir_s]);
    cmd.args(["--kernel", kernel.to_str().unwrap()]);
    cmd.args(["--disk", dir.join("disk.img").to_str().unwrap()]);
    let mut edge_fds: Vec<i32> = Vec::new();
    if m.net != "none" {
        // Every nic is the translator's (1.6.14e): spawn the
        // translator if it does not stand, connect each nic to
        // edge.sock, and hand the VMM the connected fd.
        ensure_translator(name, &dir)?;
        for (i, _nic) in m.net.split(',').enumerate() {
            let fd = connect_edge_nic(&dir, i as u8, std::time::Duration::from_secs(5))?;
            edge_fds.push(fd);
            cmd.args(["--edge-fd", &fd.to_string()]);
        }
    }
    if m.attach != "none" {
        cmd.args(["--attach-ro", &m.attach]);
    }
    cmd.args(["--mem-mb", &m.mem_mb.to_string()]);
    if cfg!(debug_assertions) {
        cmd.args(["--console", dir.join("console.sock").to_str().unwrap()]);
    }
    cmd.args(["--cmdline", &cmdline_for(&m)]);

    // From here on this is not a Command::spawn(): spawn()'s own
    // pre_exec hook cannot block waiting on its caller (the identity
    // handshake below) without deadlocking spawn() itself -- spawn()
    // does not return in the parent until the child's pre_exec
    // finishes, and the child's pre_exec does not finish until the
    // parent (past spawn()) calls newuidmap/newgidmap. cmd stays
    // useful only as an argv builder (every --bind/--unshare-* line
    // above is unchanged); the fork and exec are done by hand.
    let bwrap_path = cmd.get_program().to_owned();
    let bargs: Vec<std::ffi::CString> = std::iter::once(bwrap_path.clone())
        .chain(cmd.get_args().map(|a| a.to_owned()))
        .map(|a| std::ffi::CString::new(a.into_encoded_bytes()).expect("no NUL in an argument"))
        .collect();
    let mut argv: Vec<*const libc::c_char> = bargs.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    let bwrap_cpath = std::ffi::CString::new(bwrap_path.into_encoded_bytes())
        .map_err(|_| "the bwrap path contains a NUL byte".to_string())?;
    let ready_fd_key = std::ffi::CString::new("CELLA_READY_FD").unwrap();
    let ready_fd_val = std::ffi::CString::new(write_fd.to_string()).unwrap();
    use std::os::fd::AsRawFd;
    let console_fd = console.as_raw_fd();
    let vmm_log_fd = vmm_log.as_raw_fd();

    // SAFETY: fork() is async-signal-safe by definition; everything
    // the child branch does afterward (dup2, setsid, unshare,
    // setresuid/setresgid, raw read/write/close on this process's
    // own fds, setenv, execv) is async-signal-safe or, for setenv,
    // safe because this single-threaded child has touched no other
    // Rust allocator state since fork.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err("forking the jail failed".to_string());
    }
    if pid == 0 {
        // The child: everything here runs before exec, thus a
        // mistake here never reaches the parent's control flow --
        // any failure exits directly instead of returning.
        unsafe {
            libc::close(ready_read);
            libc::close(go_write);
            let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
            if devnull >= 0 {
                libc::dup2(devnull, 0);
                libc::close(devnull);
            }
            libc::dup2(console_fd, 1);
            libc::dup2(vmm_log_fd, 2);
            libc::setsid();
            // Unshare the user namespace ourselves, so that this
            // process -- not bwrap after another fork -- is the
            // creator. Its uid_map is still empty: no id, including
            // 0, is valid here yet, and setresuid/setresgid would
            // fail EINVAL if attempted now.
            if libc::unshare(libc::CLONE_NEWUSER) != 0 {
                libc::_exit(126);
            }
            // Signal readiness, then wait for the parent's
            // newuidmap/newgidmap calls to write the mapping from
            // outside this namespace.
            let byte = [0u8; 1];
            if libc::write(ready_write, byte.as_ptr() as *const libc::c_void, 1) != 1 {
                libc::_exit(126);
            }
            let mut buf = [0u8; 1];
            if libc::read(go_read, buf.as_mut_ptr() as *mut libc::c_void, 1) != 1 {
                libc::_exit(126);
            }
            libc::close(ready_write);
            libc::close(go_read);
            // The mapping now exists (namespace uid/gid 0 -> this
            // machine's host uid/gid): claim it. This process is
            // still the creator, thus still capable of the id
            // change even though its real uid has no entry of its
            // own in the map.
            if libc::setresgid(0, 0, 0) != 0 {
                libc::_exit(126);
            }
            if libc::setresuid(0, 0, 0) != 0 {
                libc::_exit(126);
            }
            libc::setenv(ready_fd_key.as_ptr(), ready_fd_val.as_ptr(), 1);
            // The two pipe write ends are CLOEXEC so sibling spawns
            // (the translator) cannot hold them open; this child is
            // the one process that must carry them across exec.
            for fd in [write_fd, info_write] {
                let flags = libc::fcntl(fd, libc::F_GETFD, 0);
                libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            libc::execv(bwrap_cpath.as_ptr(), argv.as_ptr());
            // execv only returns on failure.
            libc::_exit(127);
        }
    }
    // The parent, from here on.
    // SAFETY: these are this process's ends; the child holds its own.
    unsafe {
        libc::close(write_fd);
        libc::close(info_write);
        libc::close(ready_write);
        libc::close(go_read);
        // The edge fds crossed to the child at fork; the parent's
        // copies close so the translator sees exactly one holder
        // per nic (a lingering verb-side copy would mask a VMM
        // exit from the translator's EOF).
        for fd in &edge_fds {
            libc::close(*fd);
        }
    }
    // Wait for the child's readiness (it has unshared its own user
    // namespace and claimed uid/gid 0), then map that identity to
    // this machine's distinct host uid with the setuid-root helpers
    // that honor the delegated /etc/subuid and /etc/subgid range,
    // then release the child -- only then does it exec bwrap.
    let mut byte = [0u8; 1];
    // SAFETY: byte is a valid one-byte buffer; ready_read is this
    // process's own fd.
    let got_ready = unsafe { libc::read(ready_read, byte.as_mut_ptr() as *mut libc::c_void, 1) };
    // SAFETY: ready_read is this process's own fd.
    unsafe { libc::close(ready_read) };
    let reap = || {
        // SAFETY: pid is this function's own child.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let mut status = 0;
        // SAFETY: status is a valid out-param.
        unsafe { libc::waitpid(pid, &mut status, 0) };
    };
    if got_ready != 1 {
        reap();
        // SAFETY: go_write is this process's own fd.
        unsafe { libc::close(go_write) };
        return Err(format!(
            "machine {name:?} never signaled readiness for the identity mapping"
        ));
    }
    let child_pid = pid.to_string();
    if root {
        // Root maps the namespace itself: deny setgroups, then the
        // one-line gid and uid maps, 0 -> 0.
        for (file, content) in [
            ("setgroups", "deny\n".to_string()),
            ("gid_map", format!("0 {host_gid} 1\n")),
            ("uid_map", format!("0 {host_uid} 1\n")),
        ] {
            if let Err(e) = fs::write(format!("/proc/{child_pid}/{file}"), content) {
                // SAFETY: go_write is this process's own fd.
                unsafe { libc::close(go_write) };
                reap();
                return Err(format!("writing the child's {file} as root: {e}"));
            }
        }
    }
    for (tool, target) in if root {
        vec![]
    } else {
        vec![("newuidmap", host_uid), ("newgidmap", host_gid)]
    } {
        let status = std::process::Command::new(find_program(tool))
            .args([&child_pid, "0", &target.to_string(), "1"])
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            // SAFETY: go_write is this process's own fd.
            unsafe { libc::close(go_write) };
            reap();
            return Err(format!(
                "{tool} {child_pid} 0 {target} 1 failed ({status:?}) -- is {tool} \
                 installed with the setuid capability, and does a delegated sub-id \
                 range exist for this user (see docs/LIFECYCLE.md)?"
            ));
        }
    }
    // SAFETY: go_write is this process's own fd, and the write wakes
    // the child's blocking read.
    unsafe {
        libc::write(go_write, [0u8].as_ptr() as *const libc::c_void, 1);
        libc::close(go_write);
    }
    // Read the info JSON from bwrap and take child-pid: the host pid
    // of the VMM.
    let mut info = String::new();
    {
        use std::io::Read;
        // SAFETY: info_read is an owned fd, and from_raw_fd takes it.
        let mut f = unsafe { <fs::File as std::os::fd::FromRawFd>::from_raw_fd(info_read) };
        let _ = f.read_to_string(&mut info);
    }
    let pid: i32 = json_field(&info, "child-pid")
        .and_then(|v| v.parse().ok())
        .unwrap_or(pid);
    write_atomic(&pid_path(name), format!("{pid}\n").as_bytes())
        .map_err(|e| format!("writing the pid file: {e}"))?;

    // Wait for readiness: one byte on the pipe, or the death of the
    // child, or the timeout.
    let mut pfd = libc::pollfd {
        fd: read_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: pfd is valid for the duration of the call.
    let ready = unsafe { libc::poll(&mut pfd, 1, 60_000) };
    let mut byte = [0u8; 8];
    // SAFETY: byte is a valid writable buffer.
    let n = if ready > 0 {
        unsafe { libc::read(read_fd, byte.as_mut_ptr() as *mut libc::c_void, byte.len()) }
    } else {
        0
    };
    // SAFETY: read_fd is this process's fd.
    unsafe { libc::close(read_fd) };
    if n <= 0 {
        let _ = fs::remove_file(pid_path(name));
        let tail = fs::read_to_string(dir.join("vmm.log"))
            .map(|s| {
                s.lines()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        // SAFETY: pid names the child this function spawned.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        return Err(format!(
            "machine {name:?} did not reach readiness -- last vmm.log lines:\n{tail}"
        ));
    }
    println!("cella: machine {name:?} {done_word} (pid {pid})");
    Ok(())
}

/// Stop the machine as fast as possible, and clear the transients.
/// An emergency maneuver: SIGKILL, no grace. On a machine that is not
/// running, the verb still clears leftovers, thus stop is also the
/// recovery from a crash. A frozen machine is refused: its sidecar is
/// not a transient.
pub fn stop(name: &str) -> Result<(), String> {
    if !machine_dir(name).exists() {
        return Err(format!("no machine named {name:?}"));
    }
    if is_frozen(name) && !is_running(name) {
        return Err(format!(
            "machine {name:?} is frozen -- thaw it, or destroy it"
        ));
    }
    if is_running(name) {
        let pid: i32 = fs::read_to_string(pid_path(name))
            .map_err(|e| format!("reading the pid file: {e}"))?
            .trim()
            .parse()
            .map_err(|_| "the pid file is not a number".to_string())?;
        // SAFETY: pid comes from the machine's own pid file.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        for _ in 0..200 {
            // SAFETY: signal 0 probes only.
            if unsafe { libc::kill(pid, 0) } != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        println!("cella: machine {name:?} stopped");
    } else {
        println!("cella: machine {name:?} was not running");
    }
    let cleared = clear_transients(name);
    if !cleared.is_empty() {
        println!("cella: cleared transients: {}", cleared.join(", "));
    }
    Ok(())
}

// --- enter ------------------------------------------------------------

/// Attach the terminal to the serial console of the running machine.
/// Ctrl-] detaches (the virsh convention). With a pipe on stdin (the
/// tests), raw mode is skipped, and the end of the input detaches
/// after a short drain, so that the caller can read the response from
/// console.log.
/// Connect to the console socket of a running machine. A unix socket
/// path caps at ~108 bytes: connect by the file name from inside the
/// machine directory, thus any home path works.
pub fn connect_console(name: &str) -> Result<std::os::unix::net::UnixStream, String> {
    if !is_running(name) {
        return Err(format!(
            "machine {name:?} is not running -- start it (or thaw it)"
        ));
    }
    let dir = machine_dir(name);
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    std::env::set_current_dir(&dir).map_err(|e| format!("entering {}: {e}", dir.display()))?;
    let connected = std::os::unix::net::UnixStream::connect("console.sock");
    std::env::set_current_dir(&cwd).map_err(|e| e.to_string())?;
    connected.map_err(|e| format!("connecting to {}: {e}", dir.join("console.sock").display()))
}

pub fn enter(name: &str) -> Result<(), String> {
    if !cfg!(debug_assertions) {
        return Err(
            "enter is a debug affordance -- a release machine is dark: no console \
             exists, and the machine is observed through files, verbs, and the \
             chronicle"
                .to_string(),
        );
    }
    refuse_rock(name, "enter")?;
    use std::io::{Read, Write};
    let mut stream = connect_console(name)?;
    stream.set_nonblocking(true).map_err(|e| e.to_string())?;

    // SAFETY: isatty on fd 0 reads a fact.
    let tty = unsafe { libc::isatty(0) } == 1;
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    if tty {
        // SAFETY: tcgetattr/cfmakeraw/tcsetattr on the owned terminal.
        unsafe {
            libc::tcgetattr(0, &mut saved);
            let mut raw = saved;
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(0, libc::TCSANOW, &raw);
        }
        eprintln!("(connected to {name:?} -- detach: Ctrl-])\r");
    }
    // Replay the recent console output, so that the prompt of the
    // guest is visible at attach. Reading the log touches nothing in
    // the guest: an attach must not inject input.
    {
        let log = read_lossy(&machine_dir(name).join("console.log"));
        let tail: String = log
            .lines()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\r\n");
        let mut out = std::io::stdout();
        let _ = out.write_all(tail.as_bytes());
        let _ = out.flush();
    }
    // SAFETY: fcntl on stdin, restored implicitly at exit.
    unsafe {
        let flags = libc::fcntl(0, libc::F_GETFL);
        libc::fcntl(0, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }

    let mut stdin_open = true;
    let mut drain_until: Option<std::time::Instant> = None;
    // An exit of the guest shell detaches: the init of the image
    // prints this marker when the shell ends, and a fresh shell
    // respawns behind it for the next enter.
    let mut marker_window = Vec::new();
    const EXIT_MARKER: &[u8] = b"cella-shell: getty generation";
    let result = loop {
        let mut fds = [
            libc::pollfd {
                fd: 0,
                events: if stdin_open { libc::POLLIN } else { 0 },
                revents: 0,
            },
            libc::pollfd {
                fd: std::os::fd::AsRawFd::as_raw_fd(&stream),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: fds is valid for the duration of the call.
        unsafe { libc::poll(fds.as_mut_ptr(), 2, 100) };

        let mut buf = [0u8; 512];
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            match stream.read(&mut buf) {
                Ok(0) => break Ok(()), // the machine went away
                Ok(n) => {
                    let mut out = std::io::stdout();
                    let _ = out.write_all(&buf[..n]);
                    let _ = out.flush();
                    // Scan across chunk boundaries for the exit marker.
                    marker_window.extend_from_slice(&buf[..n]);
                    let win = String::from_utf8_lossy(&marker_window);
                    if win
                        .lines()
                        .any(|l| l.contains("getty generation") && l.contains("exited"))
                    {
                        break Ok(());
                    }
                    let keep = marker_window.len().saturating_sub(EXIT_MARKER.len() + 64);
                    marker_window.drain(..keep);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break Ok(()),
            }
        }
        if stdin_open && fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            // SAFETY: reading stdin into a valid buffer.
            let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n > 0 {
                let chunk = &buf[..n as usize];
                if tty && chunk.contains(&0x1d) {
                    break Ok(()); // Ctrl-]
                }
                if stream.write_all(chunk).is_err() {
                    break Ok(());
                }
            } else if n == 0 {
                stdin_open = false;
                drain_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(700));
            }
        }
        if let Some(t) = drain_until {
            if std::time::Instant::now() >= t {
                break Ok(());
            }
        }
    };
    if tty {
        // SAFETY: restoring the saved terminal state.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &saved) };
        eprintln!("\n(detached)");
    }
    result
}

// --- selftest ---------------------------------------------------------

/// The lifecycle cycle as a verb: create, start, freeze, thaw, stop,
/// restart, destroy, with every refusal checked, in a sandboxed home.
/// The golden artifacts come from the real home, and build seeds them
/// from dist/ when the current directory is the repository. Prints
/// SKIP and succeeds when the machine cannot run here (no /dev/kvm,
/// no bwrap, no goldens): the callers include the smoke battery,
/// which must degrade gracefully.
/// Serial output is not guaranteed UTF-8: the guest console emits a
/// stray byte at the 8250 init, and read_to_string fails the entire
/// read on it. Decode lossily -- the same lesson as the probes.
pub fn read_lossy(p: &Path) -> String {
    fs::read(p)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

// --- list and info ----------------------------------------------------

/// The observable state of a machine, from facts on disk.
pub fn state_of(name: &str) -> &'static str {
    if is_archived(name) {
        "archived"
    } else if is_running(name) {
        "running"
    } else if is_frozen(name) {
        "frozen"
    } else {
        "created"
    }
}

/// A named digest field from the raw manifest text (the universe
/// family records them), shortened for a column; "-" when absent.
pub fn short_digest(name: &str, key: &str) -> String {
    fs::read_to_string(machine_dir(name).join("manifest.json"))
        .ok()
        .and_then(|s| json_field(&s, key).map(|v| v.chars().take(12).collect()))
        .unwrap_or_else(|| "-".to_string())
}

/// One line per machine, the arrangement of docker and podman ps.
pub fn list() -> Result<(), String> {
    let dir = home().join("machines");
    let mut names: Vec<String> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().join("manifest.json").is_file())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    println!(
        "{:<20} {:<9} {:>7} {:>7}  {:<10} {:<5} {:<10} {:<10} {:<12}",
        "NAME", "STATE", "PID", "MEM", "NET", "ROOT", "KERNEL", "ROOTFS", "DISK-SHA3"
    );
    for name in names {
        let Ok(m) = read_manifest(&name) else {
            println!("{name:<20} (unreadable manifest)");
            continue;
        };
        let pid = fs::read_to_string(pid_path(&name))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "-".to_string());
        println!(
            "{:<20} {:<9} {:>7} {:>6}M  {:<10} {:<5} {:<10} {:<10} {:<12}",
            name,
            state_of(&name),
            if is_running(&name) { pid } else { "-".into() },
            m.mem_mb,
            m.net,
            m.root,
            m.kernel,
            m.rootfs,
            short_digest(&name, "digest_disk")
        );
    }
    Ok(())
}

/// Everything about one machine: the manifest, the state, and the
/// files with their sizes.
pub fn info(name: &str) -> Result<(), String> {
    let m = read_manifest(name)?;
    let dir = machine_dir(name);
    println!("name:    {}", m.name);
    println!("state:   {}", state_of(name));
    if m.net != "none" {
        // The valve automaton's posture: its own record, its own
        // controller (the gateway verbs), shown beside the
        // machine automaton's state.
        println!("valve:   {}", valve_record(name));
    }
    // The layer digests, where a universe operation recorded them.
    if let Ok(raw) = fs::read_to_string(dir.join("manifest.json")) {
        for key in ["digest_disk", "digest_ram"] {
            if let Some(v) = json_field(&raw, key) {
                println!("{key}: {v}");
            }
        }
    }
    if is_frozen(name) {
        if let Ok(md) = fs::metadata(dir.join("state")) {
            if let Ok(t) = md.modified() {
                let secs = t
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                println!("frozen:  since epoch {secs} (the mtime of the sidecar)");
            }
        }
    }
    if is_running(name) {
        if let Ok(pid) = fs::read_to_string(pid_path(name)) {
            println!("pid:     {}", pid.trim());
        }
    }
    println!(
        "kernel:  {} ({})",
        m.kernel,
        kernel_path(&m.kernel).display()
    );
    println!("rootfs:  {}", m.rootfs);
    println!("mem_mb:  {}", m.mem_mb);
    println!("net:     {}", m.net);
    println!("root:    {}", m.root);
    println!("dir:     {}", dir.display());
    println!("files:");
    let mut entries: Vec<_> = fs::read_dir(&dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let size = e.metadata().map(|md| md.len()).unwrap_or(0);
        println!(
            "  {:<16} {:>12} bytes",
            e.file_name().to_string_lossy(),
            size
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_home<F: FnOnce()>(f: F) {
        // The environment is process-global and the tests run in
        // parallel threads: serialize every test that sets CELLA_HOME.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "cella-machine-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("CELLA_HOME", &dir);
        f();
        std::env::remove_var("CELLA_HOME");
        let _ = fs::remove_dir_all(&dir);
    }

    fn sample() -> Manifest {
        Manifest {
            name: "m1".into(),
            kernel: "canonical".into(),
            rootfs: "canonical".into(),
            mem_mb: 256,
            net: "none".into(),
            root: "rw".into(),
            diag: "off".into(),
            attach: "none".into(),
        }
    }

    #[test]
    fn manifest_round_trips() {
        let m = sample();
        assert_eq!(Manifest::from_json(&m.to_json()).unwrap(), m);
    }

    #[test]
    fn names_are_path_safe() {
        assert!(valid_name("m1"));
        assert!(valid_name("agent-7"));
        assert!(!valid_name(""));
        assert!(!valid_name("-x"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("A"));
        assert!(!valid_name(".."));
    }

    #[test]
    fn defaults_read_config_and_flags_win() {
        with_temp_home(|| {
            let d = defaults();
            assert_eq!(
                (d.kernel.as_str(), d.rootfs.as_str()),
                ("canonical", "cella")
            );
            fs::write(
                home().join("config.json"),
                "{\n  \"rootfs\": \"canonical\",\n  \"mem_mb\": 128\n}\n",
            )
            .unwrap();
            let d = defaults();
            assert_eq!(d.rootfs, "canonical");
            assert_eq!(d.mem_mb, 128);
            assert_eq!(d.kernel, "canonical"); // absent field keeps the built-in
        });
    }

    #[test]
    fn the_net_grammar_refuses_what_it_does_not_name() {
        with_temp_home(|| {
            for p in [kernel_path("canonical"), rootfs_path("canonical")] {
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(&p, b"fake").unwrap();
            }
            for (net, want) in [
                ("tap0", "want world"),
                ("auto", "want world"),
                ("wire:Bad_Name", "lowercase"),
                ("world:80/tls", "PORT/tcp or PORT/udp"),
                ("world:notaport/tcp", "PORT/tcp or PORT/udp"),
            ] {
                let mut m = sample();
                m.net = net.into();
                let err = create(&m).unwrap_err();
                assert!(err.contains(want), "{net}: {err}");
            }
            for net in [
                "none",
                "world",
                "world:1709/tcp+53/udp",
                "wire:ab",
                "world,wire:ab",
            ] {
                let mut m = sample();
                m.net = net.into();
                create(&m).unwrap_or_else(|e| panic!("{net}: {e}"));
                destroy("m1").unwrap();
            }
        });
    }

    #[test]
    fn create_requires_goldens_and_destroy_removes() {
        with_temp_home(|| {
            let m = sample();
            let err = create(&m).unwrap_err();
            assert!(err.contains("cella build kernel canonical"), "{err}");

            // Stage fake goldens, then the cycle works.
            for p in [kernel_path("canonical"), rootfs_path("canonical")] {
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(&p, b"fake").unwrap();
            }
            create(&m).unwrap();
            assert!(machine_dir("m1").join("disk.img").is_file());
            assert_eq!(read_manifest("m1").unwrap(), m);
            let err = create(&m).unwrap_err();
            assert!(err.contains("already exists"), "{err}");
            destroy("m1").unwrap();
            assert!(!machine_dir("m1").exists());
            assert!(destroy("m1").is_err());
        });
    }
}
