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
        fn field<'a>(s: &'a str, key: &str) -> Result<&'a str, String> {
            let pat = format!("\"{key}\":");
            let i = s.find(&pat).ok_or_else(|| format!("missing field {key}"))?;
            let rest = s[i + pat.len()..].trim_start();
            if let Some(r) = rest.strip_prefix('"') {
                r.split('"')
                    .next()
                    .ok_or_else(|| format!("bad field {key}"))
            } else {
                Ok(rest
                    .split(|c: char| c == ',' || c == '}' || c.is_whitespace())
                    .next()
                    .unwrap_or(""))
            }
        }
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

/// Stage a machine: verify the goldens, copy the rootfs flavor to the
/// machine's own disk, and write the manifest. No process starts.
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
        let dir = std::env::temp_dir().join(format!("cella-machine-test-{}", std::process::id()));
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

/// Start the machine: spawn the VMM inside the jail, detached, and
/// return when the VMM signals readiness (immediately before the
/// first KVM_RUN). The jail is the bwrap invocation of
/// scripts/jail.sh, spawned directly with no shell. One deliberate
/// difference: no --die-with-parent. The verb process exits after
/// readiness, and the machine must survive it; the pid file and stop
/// own the cleanup instead.
pub fn start(name: &str) -> Result<(), String> {
    let m = read_manifest(name)?;
    if is_running(name) {
        return Err(format!("machine {name:?} is already running"));
    }
    if is_frozen(name) {
        return Err(format!("machine {name:?} is frozen -- thaw it"));
    }
    clear_transients(name);
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
    // SAFETY: write_fd is this process's end; the child holds its own.
    unsafe { libc::close(write_fd) };
    let pid = child.id() as i32;
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
    println!("cella: machine {name:?} running (pid {pid})");
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
