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
