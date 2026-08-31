//! The native build of the golden artifacts. Rust orchestrates: the
//! downloads and the extractions run as direct commands, the compile
//! steps run inside the cella-build toolbox one command at a time,
//! and the configuration checks run here. No shell script sits in
//! the middle; see docs/LIFECYCLE.md, "The security boundary".
//!
//! The compile toolchain itself (make, gcc) lives in the toolbox and
//! stays out of the host, exactly as before. A build runs before any
//! guest exists, thus it runs outside the runtime confinement.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The pinned versions, in one place. The Makefile carries the same
/// pins for the remaining script-driven flavors during the migration.
pub const KERNEL_VERSION: &str = "7.2.2";

fn repo_root() -> PathBuf {
    // The build reads its fragments from the repository. A build from
    // an installed binary needs the repository checkout as the
    // current directory, and says so when the fragments are absent.
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Run one command, inherit the output, and fail loudly.
fn run(what: &str, program: &str, args: &[&str], cwd: Option<&Path>) -> Result<(), String> {
    let mut c = Command::new(program);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let status = c
        .status()
        .map_err(|e| format!("{what}: spawning {program}: {e}"))?;
    if !status.success() {
        return Err(format!("{what}: {program} failed ({status})"));
    }
    Ok(())
}

/// Run one command inside the cella-build toolbox.
fn run_in_toolbox(what: &str, cwd: &Path, args: &[&str]) -> Result<(), String> {
    let mut full: Vec<&str> = vec!["run", "-c", "cella-build"];
    full.extend_from_slice(args);
    run(what, "toolbox", &full, Some(cwd))
}

/// Download with resume and retries, then extract. The cache lives in
/// target/, the same trees as the script era, thus a source that is
/// already present downloads nothing.
fn fetch_and_extract(
    what: &str,
    url: &str,
    tarball: &Path,
    extracted: &Path,
    into: &Path,
) -> Result<(), String> {
    if extracted.is_dir() {
        println!("cella: {what}: source already present, skipping the download");
        return Ok(());
    }
    fs::create_dir_all(into).map_err(|e| e.to_string())?;
    println!("cella: {what}: downloading {url}");
    run(
        what,
        "curl",
        &[
            "-fL",
            "--progress-bar",
            "--retry",
            "5",
            "--retry-delay",
            "2",
            "--retry-all-errors",
            "-C",
            "-",
            "-o",
            tarball.to_str().unwrap(),
            url,
        ],
        None,
    )?;
    run(
        what,
        "tar",
        &[
            "-xf",
            tarball.to_str().unwrap(),
            "-C",
            into.to_str().unwrap(),
        ],
        None,
    )?;
    let _ = fs::remove_file(tarball);
    Ok(())
}

/// Assert that a resolved kernel config carries a symbol as =y.
/// kconfig overrules a fragment silently, and the failure mode that
/// matters (a cut serial console) produces a kernel that boots and
/// says nothing.
fn assert_config(config: &Path, symbols: &[&str]) -> Result<(), String> {
    let text = fs::read_to_string(config).map_err(|e| format!("reading .config: {e}"))?;
    for sym in symbols {
        if !text.contains(&format!("{sym}=y")) {
            return Err(format!(
                "{sym} did not survive the config resolution -- the fragment and the defconfig disagree"
            ));
        }
    }
    Ok(())
}

/// The canonical kernel, natively: download the pinned source, merge
/// the fragment onto the defconfig, assert the result, compile, and
/// place the golden. The output also lands in dist/ while the
/// migration lasts: the probes still pin there.
pub fn kernel_canonical(golden: &Path) -> Result<(), String> {
    let root = repo_root();
    let fragment = root.join("scripts/build/kernel-fragment.config");
    if !fragment.is_file() {
        return Err(format!(
            "{} missing -- run the build from the repository checkout",
            fragment.display()
        ));
    }
    let kbuild = root.join("target/kernel-build");
    let src = kbuild.join(format!("linux-{KERNEL_VERSION}"));
    let major = KERNEL_VERSION.split('.').next().unwrap();
    fetch_and_extract(
        "kernel",
        &format!(
            "https://cdn.kernel.org/pub/linux/kernel/v{major}.x/linux-{KERNEL_VERSION}.tar.xz"
        ),
        &kbuild.join(format!("linux-{KERNEL_VERSION}.tar.xz")),
        &src,
        &kbuild,
    )?;

    println!("cella: kernel: configuring (defconfig + fragment)");
    run_in_toolbox("kernel config", &src, &["make", "x86_64_defconfig"])?;
    run_in_toolbox(
        "kernel merge",
        &src,
        &[
            "scripts/kconfig/merge_config.sh",
            "-m",
            ".config",
            fragment.to_str().unwrap(),
        ],
    )?;
    run_in_toolbox("kernel oldconfig", &src, &["make", "olddefconfig"])?;
    assert_config(
        &src.join(".config"),
        &[
            "CONFIG_TTY",
            "CONFIG_SERIAL_8250",
            "CONFIG_SERIAL_8250_CONSOLE",
            "CONFIG_VIRTIO_MMIO",
            "CONFIG_VIRTIO_BLK",
            "CONFIG_KVM_GUEST",
            "CONFIG_DEVTMPFS_MOUNT",
        ],
    )?;

    println!("cella: kernel: building bzImage");
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .to_string();
    run_in_toolbox("kernel build", &src, &["make", "-j", &jobs, "bzImage"])?;

    let built = src.join("arch/x86/boot/bzImage");
    fs::create_dir_all(golden.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = golden.with_extension("tmp");
    fs::copy(&built, &tmp).map_err(|e| format!("copying the kernel: {e}"))?;
    fs::rename(&tmp, golden).map_err(|e| e.to_string())?;
    // The proofs still pin against dist/ during the migration.
    let dist = root.join("dist/bzImage");
    if dist.parent().unwrap().is_dir() {
        let _ = fs::copy(&built, &dist);
    }
    println!("cella: golden kernel canonical -> {}", golden.display());
    Ok(())
}
