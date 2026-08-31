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
    /// A TAP device name, or "none".
    pub net: String,
    /// "rw" or "ro".
    pub root: String,
}

/// Read one field of a flat JSON object: a quoted string or a bare
/// number. Returns None when the key is absent.
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
    };
    let path = home().join("config.json");
    let Ok(s) = fs::read_to_string(&path) else {
        // Seed the file with the built-ins on first use, so that the
        // knobs are discoverable by reading it. A seeded file changes
        // nothing: its values are the built-ins.
        let seed = format!(
            "{{\n  \"kernel\": \"{}\",\n  \"rootfs\": \"{}\",\n  \"mem_mb\": {},\n  \"net\": \"{}\",\n  \"root\": \"{}\"\n}}\n",
            m.kernel, m.rootfs, m.mem_mb, m.net, m.root
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
    m
}

impl Manifest {
    pub fn to_json(&self) -> String {
        format!(
            "{{\n  \"name\": \"{}\",\n  \"kernel\": \"{}\",\n  \"rootfs\": \"{}\",\n  \"mem_mb\": {},\n  \"net\": \"{}\",\n  \"root\": \"{}\"\n}}\n",
            self.name, self.kernel, self.rootfs, self.mem_mb, self.net, self.root
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

pub fn read_manifest(name: &str) -> Result<Manifest, String> {
    let p = machine_dir(name).join("manifest.json");
    let s = fs::read_to_string(&p).map_err(|e| format!("reading {}: {e}", p.display()))?;
    Manifest::from_json(&s)
}

/// The tap devices that the existing machines claim. The manifest is
/// the record of an allocation: one tap belongs to one machine, from
/// create to destroy.
fn claimed_taps() -> Vec<String> {
    let Ok(entries) = fs::read_dir(home().join("machines")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let s = fs::read_to_string(e.path().join("manifest.json")).ok()?;
            let net = json_field(&s, "net")?.to_string();
            (net != "none").then_some(net)
        })
        .collect()
}

/// Allocate the lowest free tap: present on the host, and claimed by
/// no machine.
fn allocate_tap() -> Result<String, String> {
    let claimed = claimed_taps();
    let mut taps: Vec<(u32, String)> = fs::read_dir("/sys/class/net")
        .map_err(|e| format!("listing /sys/class/net: {e}"))?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let n: u32 = name.strip_prefix("tap")?.parse().ok()?;
            Some((n, name))
        })
        .collect();
    taps.sort();
    taps.into_iter()
        .map(|(_, name)| name)
        .find(|t| !claimed.contains(t))
        .ok_or_else(|| {
            "no free tap in the pool -- run: make setup-tap (or free one with destroy)".to_string()
        })
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
    let mut m = m.clone();
    if m.net == "auto" {
        m.net = allocate_tap()?;
    } else if m.net != "none" && claimed_taps().contains(&m.net) {
        return Err(format!(
            "tap {:?} is already claimed by another machine",
            m.net
        ));
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
    fs::remove_dir_all(&dir).map_err(|e| format!("removing {}: {e}", dir.display()))
}

/// The build verb, first step: the golden artifacts come from a copy
/// of the repository's dist/, which stays the proof path. The native
/// build (Rust-orchestrated toolchain) is a later migration step; see
/// docs/LIFECYCLE.md.
pub fn build(axis: &str, flavor: &str) -> Result<(), String> {
    let (dest, src) = match (axis, flavor) {
        ("kernel", "canonical") => (kernel_path(flavor), "dist/bzImage"),
        ("kernel", "nested") => (kernel_path(flavor), "dist/bzImage-nested"),
        ("rootfs", "canonical") => (rootfs_path(flavor), "dist/rootfs.ext4"),
        ("rootfs", "cella") => (rootfs_path(flavor), "dist/rootfs-cella.ext4"),
        ("rootfs", "nested") => (rootfs_path(flavor), "dist/rootfs-nested.ext4"),
        ("rootfs", "inception") => (rootfs_path(flavor), "dist/rootfs-inception.ext4"),
        _ => {
            return Err(format!(
                "unknown build target {axis:?} {flavor:?} -- axes: kernel, rootfs; see docs/LIFECYCLE.md"
            ))
        }
    };
    let src = PathBuf::from(src);
    if !src.is_file() {
        return Err(format!(
            "{} missing -- build the proof artifacts first: make dist (or make dist-nested)",
            src.display()
        ));
    }
    let dir = dest.parent().expect("golden path has a parent");
    fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let tmp = dest.with_extension("tmp");
    fs::copy(&src, &tmp).map_err(|e| format!("copying {}: {e}", src.display()))?;
    fs::rename(&tmp, &dest).map_err(|e| format!("renaming: {e}"))?;
    println!("cella: golden {axis} {flavor} -> {}", dest.display());
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
    fn a_tap_claim_is_exclusive() {
        with_temp_home(|| {
            for p in [kernel_path("canonical"), rootfs_path("canonical")] {
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(&p, b"fake").unwrap();
            }
            let mut a = sample();
            a.net = "tap7".into();
            create(&a).unwrap();
            let mut b = sample();
            b.name = "m2".into();
            b.net = "tap7".into();
            let err = create(&b).unwrap_err();
            assert!(err.contains("already claimed"), "{err}");
            destroy("m1").unwrap();
            create(&b).unwrap(); // the destroy freed the claim
            destroy("m2").unwrap();
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

fn is_frozen(name: &str) -> bool {
    machine_dir(name).join("state").is_file()
}

/// The kernel command line of a machine, from its manifest. One
/// source: config.rs holds the defaults, and the manifest holds the
/// choices of create.
fn cmdline_for(m: &Manifest) -> String {
    let base = config::default_cmdline();
    if m.net == "none" {
        format!(
            "{base} root=/dev/vda {} virtio_mmio.device=4K@0xd0000000:5",
            m.root
        )
    } else {
        format!(
            "{base} root=/dev/vda {} virtio_mmio.device=4K@0xd0000000:5 \
             virtio_mmio.device=4K@0xd0001000:6 \
             ip={}::{}:255.255.255.0::eth0:off",
            m.root,
            config::DEFAULT_GUEST_IP,
            config::DEFAULT_HOST_IP
        )
    }
}

/// Start the machine: a fresh boot. Refuses a frozen machine, so
/// that a sidecar cannot vanish by accident; thaw is the verb for it.
pub fn start(name: &str) -> Result<(), String> {
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

/// The shared spawn of start and thaw: the VMM inside the jail,
/// detached, with readiness on a pipe. The jail is the bwrap
/// invocation of scripts/jail.sh, spawned directly with no shell.
/// One deliberate difference: no --die-with-parent. The verb process
/// exits after readiness, and the machine must survive it; the pid
/// file and stop own the cleanup instead.
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
    let bin = std::env::current_exe().map_err(|e| format!("finding the binary: {e}"))?;
    let kernel_dir = kernel.parent().expect("kernel path has a parent");

    // The readiness pipe. The child inherits the write end, and the
    // VMM writes one line on it immediately before the first KVM_RUN
    // (see CELLA_READY_FD in main.rs).
    let mut fds = [0i32; 2];
    // SAFETY: fds is a valid two-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err("creating the readiness pipe failed".to_string());
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    // The info pipe. bwrap reports the host pid of its child (the VMM)
    // on --info-fd, and that pid is the fact the pid file records: the
    // freeze signal then goes straight to the VMM, with no process
    // walking.
    let mut ifds = [0i32; 2];
    // SAFETY: ifds is a valid two-element array.
    if unsafe { libc::pipe(ifds.as_mut_ptr()) } != 0 {
        return Err("creating the info pipe failed".to_string());
    }
    let (info_read, info_write) = (ifds[0], ifds[1]);

    let console = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("console.log"))
        .map_err(|e| format!("opening console.log: {e}"))?;
    let vmm_log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("vmm.log"))
        .map_err(|e| format!("opening vmm.log: {e}"))?;

    let mut cmd = std::process::Command::new("bwrap");
    cmd.args([
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup",
    ]);
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
    cmd.args(["--ro-bind", bin.to_str().unwrap(), "/cella"]);
    ro(&mut cmd, "/lib");
    ro(&mut cmd, "/usr/lib");
    if Path::new("/lib64").is_dir() {
        ro(&mut cmd, "/lib64");
    }
    cmd.args(["--dev-bind", "/dev/kvm", "/dev/kvm"]);
    if m.net != "none" {
        cmd.args(["--dev-bind", "/dev/net/tun", "/dev/net/tun"]);
    }
    let dir_s = dir.to_str().unwrap().to_string();
    cmd.args(["--bind", &dir_s, &dir_s]);
    cmd.args([
        "--ro-bind",
        kernel_dir.to_str().unwrap(),
        kernel_dir.to_str().unwrap(),
    ]);
    cmd.arg("--new-session");
    cmd.arg("/cella");
    cmd.args(["--state-dir", &dir_s]);
    cmd.args(["--kernel", kernel.to_str().unwrap()]);
    cmd.args(["--disk", dir.join("disk.img").to_str().unwrap()]);
    if m.net != "none" {
        cmd.args(["--tap", &m.net]);
    }
    cmd.args(["--mem-mb", &m.mem_mb.to_string()]);
    cmd.args(["--console", dir.join("console.sock").to_str().unwrap()]);
    cmd.args(["--cmdline", &cmdline_for(&m)]);
    cmd.env("CELLA_READY_FD", write_fd.to_string());
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(console);
    cmd.stderr(vmm_log);
    // SAFETY: setsid in the child detaches it from this session, and
    // it calls nothing async-signal-unsafe.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    use std::os::unix::process::CommandExt;
    let child = cmd.spawn().map_err(|e| format!("spawning the jail: {e}"))?;
    // SAFETY: these are this process's ends; the child holds its own.
    unsafe {
        libc::close(write_fd);
        libc::close(info_write);
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
        .unwrap_or(child.id() as i32);
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
pub fn enter(name: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    if !is_running(name) {
        return Err(format!(
            "machine {name:?} is not running -- start it (or thaw it)"
        ));
    }
    // A unix socket path caps at ~108 bytes: connect by the file name
    // from inside the machine directory, thus any home path works.
    let dir = machine_dir(name);
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    std::env::set_current_dir(&dir).map_err(|e| format!("entering {}: {e}", dir.display()))?;
    let connected = std::os::unix::net::UnixStream::connect("console.sock");
    std::env::set_current_dir(&cwd).map_err(|e| e.to_string())?;
    let mut stream = connected
        .map_err(|e| format!("connecting to {}: {e}", dir.join("console.sock").display()))?;
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
fn read_lossy(p: &Path) -> String {
    fs::read(p)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

pub fn selftest() -> Result<(), String> {
    let kvm_ok = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok();
    if !kvm_ok {
        println!("SKIP: no read and write access to /dev/kvm");
        return Ok(());
    }
    if std::process::Command::new("bwrap")
        .arg("--version")
        .output()
        .is_err()
    {
        println!("SKIP: bwrap not found");
        return Ok(());
    }
    // Goldens: use the real home, and seed from dist/ when possible.
    for (axis, flavor) in [("kernel", "canonical"), ("rootfs", "cella")] {
        let p = if axis == "kernel" {
            kernel_path(flavor)
        } else {
            rootfs_path(flavor)
        };
        if !p.is_file() && build(axis, flavor).is_err() {
            println!("SKIP: golden {axis} {flavor} missing -- run: cella build {axis} {flavor}");
            return Ok(());
        }
    }
    let real_kernel = kernel_path("canonical");
    let real_rootfs = rootfs_path("cella");

    // A sandboxed home, so that the test disturbs no machine. The
    // goldens link in from the real home.
    let sandbox = std::env::temp_dir().join(format!("cella-selftest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&sandbox);
    fs::create_dir_all(&sandbox).map_err(|e| format!("creating the sandbox: {e}"))?;
    std::env::set_var("CELLA_HOME", &sandbox);
    for (p, src) in [
        (kernel_path("canonical"), real_kernel),
        (rootfs_path("cella"), real_rootfs),
    ] {
        fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::copy(&src, &p).map_err(|e| format!("copying a golden: {e}"))?;
    }

    let result = selftest_cycle();
    let _ = stop("m1");
    if let Err(e) = &result {
        // A failure carries its evidence: the logs stay, and their
        // tails print.
        eprintln!("selftest failed: {e}");
        for f in ["vmm.log", "console.log"] {
            let p = machine_dir("m1").join(f);
            let content = read_lossy(&p);
            let tail = content
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .join("\n  ");
            eprintln!("-- {f} (last lines, reversed):\n  {tail}");
        }
        eprintln!("(sandbox kept: {})", sandbox.display());
        std::env::remove_var("CELLA_HOME");
        return result;
    }
    std::env::remove_var("CELLA_HOME");
    let _ = fs::remove_dir_all(&sandbox);
    println!("PASS: the lifecycle cycle, with every refusal checked");
    Ok(())
}

fn selftest_cycle() -> Result<(), String> {
    fn step(what: &str, r: Result<(), String>) -> Result<(), String> {
        r.map_err(|e| format!("{what}: {e}"))
    }
    fn refuse(what: &str, r: Result<(), String>) -> Result<(), String> {
        match r {
            Ok(()) => Err(format!("{what}: accepted, and it must refuse")),
            Err(_) => Ok(()),
        }
    }
    let mut m = defaults();
    m.name = "m1".into();
    m.net = "none".into();
    step("create", create(&m))?;
    step("start", start("m1"))?;
    let console = machine_dir("m1").join("console.log");
    let mut booted = false;
    for _ in 0..200 {
        if read_lossy(&console).contains("cella-rootfs: init running") {
            booted = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !booted {
        return Err("the guest did not reach its init".to_string());
    }
    refuse("double start", start("m1"))?;
    step("freeze", freeze("m1"))?;
    if !is_frozen("m1") {
        return Err("no sidecar after the freeze".to_string());
    }
    refuse("start while frozen", start("m1"))?;
    refuse("stop while frozen", stop("m1"))?;
    step("thaw", thaw("m1"))?;
    if is_frozen("m1") {
        return Err("the sidecar survived the thaw".to_string());
    }
    refuse("thaw while running", thaw("m1"))?;
    step("stop", stop("m1"))?;
    step("restart", start("m1"))?;
    step("stop again", stop("m1"))?;
    step("destroy", destroy("m1"))?;
    Ok(())
}

// --- list and info ----------------------------------------------------

/// The observable state of a machine, from facts on disk.
pub fn state_of(name: &str) -> &'static str {
    if is_running(name) {
        "running"
    } else if is_frozen(name) {
        "frozen"
    } else {
        "created"
    }
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
        "{:<20} {:<9} {:>7} {:>7}  {:<10} {:<5} {:<10} {:<10}",
        "NAME", "STATE", "PID", "MEM", "NET", "ROOT", "KERNEL", "ROOTFS"
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
            "{:<20} {:<9} {:>7} {:>6}M  {:<10} {:<5} {:<10} {:<10}",
            name,
            state_of(&name),
            if is_running(&name) { pid } else { "-".into() },
            m.mem_mb,
            m.net,
            m.root,
            m.kernel,
            m.rootfs
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
