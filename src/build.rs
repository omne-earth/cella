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

pub fn repo_root() -> PathBuf {
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

/// The toolbox packages: the kernel and busybox toolchain, mkfs, the
/// static glibc, and rust for the crt-static in-guest binaries.
const TOOLBOX_PACKAGES: &[&str] = &[
    "gcc",
    "make",
    "bc",
    "bison",
    "flex",
    "elfutils-libelf-devel",
    "openssl-devel",
    "perl-interpreter",
    "perl-generators",
    "xz",
    "bzip2",
    "e2fsprogs",
    "glibc-static",
    "rust",
    "cargo",
    // The static bubblewrap for the in-guest jail.
    "meson",
    "ninja-build",
    "libcap-devel",
    "libcap-static",
];

/// Provision the cella-build toolbox when it is absent. The build verb
/// owns its own prerequisites: after install, no path depends on the
/// Makefile. Idempotent, and checked once per process.
fn ensure_toolbox() -> Result<(), String> {
    static DONE: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    DONE.get_or_init(|| {
        let list = Command::new("toolbox")
            .args(["list", "-c"])
            .output()
            .map_err(|e| format!("running toolbox: {e} -- install the toolbox package"))?;
        let have = String::from_utf8_lossy(&list.stdout)
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some("cella-build"));
        if !have {
            println!("cella: creating the cella-build toolbox");
            run(
                "create the toolbox",
                "toolbox",
                &["create", "-y", "cella-build"],
                None,
            )?;
        }
        println!("cella: provisioning the toolbox toolchain (idempotent)");
        let mut args: Vec<&str> = vec!["run", "-c", "cella-build", "sudo", "dnf", "install", "-y"];
        args.extend_from_slice(TOOLBOX_PACKAGES);
        let status = Command::new("toolbox")
            .args(&args)
            .stdout(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("provisioning the toolbox: {e}"))?;
        if !status.success() {
            return Err("provisioning the toolbox failed".to_string());
        }
        Ok(())
    })
    .clone()
}

/// Run one command inside the cella-build toolbox.
fn run_in_toolbox(what: &str, cwd: &Path, args: &[&str]) -> Result<(), String> {
    ensure_toolbox()?;
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
    println!("cella: golden kernel canonical -> {}", golden.display());
    Ok(())
}

pub const BUSYBOX_VERSION: &str = "1.37.0";

/// Run a toolbox command with silenced stdout and a closed stdin: the
/// kconfig steps prompt on a terminal and print pages otherwise.
fn run_in_toolbox_quiet(what: &str, cwd: &Path, args: &[&str]) -> Result<(), String> {
    ensure_toolbox()?;
    let mut full: Vec<&str> = vec!["run", "-c", "cella-build"];
    full.extend_from_slice(args);
    let status = Command::new("toolbox")
        .args(&full)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("{what}: spawning toolbox: {e}"))?;
    if !status.success() {
        return Err(format!("{what}: failed ({status})"));
    }
    Ok(())
}

/// Apply a kconfig fragment the way busybox needs it: its oldconfig
/// keeps the first definition of a symbol, thus every symbol that the
/// fragment overrides leaves the base config first, and the fragment
/// appends after.
fn apply_busybox_fragment(config: &Path, fragment: &Path) -> Result<(), String> {
    let frag = fs::read_to_string(fragment).map_err(|e| e.to_string())?;
    let symbols: Vec<String> = frag
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if let Some(rest) = l.strip_prefix("# CONFIG_") {
                rest.split(' ').next().map(|s| format!("CONFIG_{s}"))
            } else if l.starts_with("CONFIG_") {
                l.split('=').next().map(str::to_string)
            } else {
                None
            }
        })
        .collect();
    let base = fs::read_to_string(config).map_err(|e| e.to_string())?;
    let kept: Vec<&str> = base
        .lines()
        .filter(|l| {
            !symbols
                .iter()
                .any(|sym| *l == format!("# {sym} is not set") || l.starts_with(&format!("{sym}=")))
        })
        .collect();
    fs::write(config, format!("{}\n{frag}", kept.join("\n"))).map_err(|e| e.to_string())
}

/// The canonical rootfs, natively: a static busybox from pinned
/// source, the heartbeat init, hardlinked applets, one ext4 image.
pub fn rootfs_canonical(golden: &Path) -> Result<(), String> {
    let root = repo_root();
    let init = root.join("scripts/build/rootfs.sh");
    let fragment = root.join("scripts/build/busybox-fragment.config");
    for f in [&init, &fragment] {
        if !f.is_file() {
            return Err(format!(
                "{} missing -- run the build from the repository checkout",
                f.display()
            ));
        }
    }
    let rbuild = root.join("target/rootfs-build");
    let src = rbuild.join(format!("busybox-{BUSYBOX_VERSION}"));
    fetch_and_extract(
        "busybox",
        &format!("https://busybox.net/downloads/busybox-{BUSYBOX_VERSION}.tar.bz2"),
        &rbuild.join(format!("busybox-{BUSYBOX_VERSION}.tar.bz2")),
        &src,
        &rbuild,
    )?;

    println!("cella: busybox: configuring (defconfig + fragment)");
    run_in_toolbox_quiet("busybox defconfig", &src, &["make", "defconfig"])?;
    apply_busybox_fragment(&src.join(".config"), &fragment)?;
    run_in_toolbox_quiet("busybox oldconfig", &src, &["make", "oldconfig"])?;
    println!("cella: busybox: building");
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .to_string();
    run_in_toolbox_quiet("busybox build", &src, &["make", "-j", &jobs, "busybox"])?;

    println!("cella: rootfs: assembling");
    let rootdir = rbuild.join("root");
    let _ = fs::remove_dir_all(&rootdir);
    // tmp is a mountpoint: the variant inits mount a tmpfs on it, and
    // a read-only root cannot create it at run time.
    for d in ["bin", "sbin", "proc", "sys", "dev", "tmp"] {
        fs::create_dir_all(rootdir.join(d)).map_err(|e| e.to_string())?;
    }
    fs::copy(src.join("busybox"), rootdir.join("bin/busybox")).map_err(|e| e.to_string())?;
    // Hardlinks, not symlinks: busybox --install writes symlink targets
    // as the absolute invocation path, which does not exist inside the
    // guest. The built busybox is a static x86_64 binary, thus it
    // installs its own applets right here.
    run(
        "busybox install",
        rootdir.join("bin/busybox").to_str().unwrap(),
        &["--install", rootdir.join("bin").to_str().unwrap()],
        None,
    )?;
    fs::copy(&init, rootdir.join("sbin/init")).map_err(|e| e.to_string())?;
    let mut perm = fs::metadata(rootdir.join("sbin/init"))
        .map_err(|e| e.to_string())?
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(rootdir.join("sbin/init"), perm).map_err(|e| e.to_string())?;

    let img = rbuild.join("rootfs.ext4");
    let _ = fs::remove_file(&img);
    let f = fs::File::create(&img).map_err(|e| e.to_string())?;
    f.set_len(16 * 1024 * 1024).map_err(|e| e.to_string())?;
    drop(f);
    run_in_toolbox_quiet(
        "mkfs",
        &rbuild,
        &[
            "mkfs.ext4",
            "-q",
            "-F",
            "-d",
            rootdir.to_str().unwrap(),
            img.to_str().unwrap(),
        ],
    )?;

    fs::create_dir_all(golden.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = golden.with_extension("tmp");
    fs::copy(&img, &tmp).map_err(|e| e.to_string())?;
    fs::rename(&tmp, golden).map_err(|e| e.to_string())?;
    println!("cella: golden rootfs canonical -> {}", golden.display());
    Ok(())
}

pub const GUEST_BASH_VERSION: &str = "5.3";

/// The interactive rootfs, natively: the canonical root plus a real
/// static bash and the interactive init. Builds the canonical tree
/// first when it is absent: the flavor extends it.
pub fn rootfs_cella(golden: &Path, canonical_golden: &Path) -> Result<(), String> {
    let root = repo_root();
    let init = root.join("scripts/build/rootfs-cella.sh");
    if !init.is_file() {
        return Err(format!(
            "{} missing -- run the build from the repository checkout",
            init.display()
        ));
    }
    let rbuild = root.join("target/rootfs-build");
    let rootdir = rbuild.join("root");
    if !rootdir.is_dir() {
        println!("cella: rootfs cella: the canonical tree is absent, building it first");
        rootfs_canonical(canonical_golden)?;
    }

    let bsrc = rbuild.join(format!("bash-{GUEST_BASH_VERSION}"));
    fetch_and_extract(
        "bash",
        &format!("https://ftp.gnu.org/gnu/bash/bash-{GUEST_BASH_VERSION}.tar.gz"),
        &rbuild.join(format!("bash-{GUEST_BASH_VERSION}.tar.gz")),
        &bsrc,
        &rbuild,
    )?;
    if !bsrc.join("bash").is_file() {
        println!("cella: bash: configuring and building (static)");
        run_in_toolbox_quiet(
            "bash configure",
            &bsrc,
            &[
                "./configure",
                "--enable-static-link",
                "--without-bash-malloc",
            ],
        )?;
        let jobs = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .to_string();
        run_in_toolbox_quiet("bash build", &bsrc, &["make", "-j", &jobs])?;
    }

    println!("cella: rootfs cella: assembling");
    let croot = rbuild.join("root-cella");
    let _ = fs::remove_dir_all(&croot);
    run(
        "copy the root",
        "cp",
        &["-a", rootdir.to_str().unwrap(), croot.to_str().unwrap()],
        None,
    )?;
    for (from, to, mode) in [
        (bsrc.join("bash"), croot.join("bin/bash"), 0o755),
        (init.clone(), croot.join("sbin/init"), 0o755),
    ] {
        fs::copy(&from, &to).map_err(|e| e.to_string())?;
        let mut p = fs::metadata(&to).map_err(|e| e.to_string())?.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut p, mode);
        fs::set_permissions(&to, p).map_err(|e| e.to_string())?;
    }

    let img = rbuild.join("rootfs-cella.ext4");
    let _ = fs::remove_file(&img);
    let f = fs::File::create(&img).map_err(|e| e.to_string())?;
    f.set_len(16 * 1024 * 1024).map_err(|e| e.to_string())?;
    drop(f);
    run_in_toolbox_quiet(
        "mkfs",
        &rbuild,
        &[
            "mkfs.ext4",
            "-q",
            "-F",
            "-d",
            croot.to_str().unwrap(),
            img.to_str().unwrap(),
        ],
    )?;
    fs::create_dir_all(golden.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = golden.with_extension("tmp");
    fs::copy(&img, &tmp).map_err(|e| e.to_string())?;
    fs::rename(&tmp, golden).map_err(|e| e.to_string())?;
    println!("cella: golden rootfs cella -> {}", golden.display());
    Ok(())
}

/// The gateway rootfs: the canonical tree with the gateway init --
/// the appliance between an agent and the world (see TASKS.md and
/// docs/LIFECYCLE.md). No bash, no diagnostics beyond a heartbeat:
/// the appliance forwards, and a busybox shell serves diagnosis.
pub fn rootfs_gateway(golden: &Path, canonical_golden: &Path) -> Result<(), String> {
    let root = repo_root();
    let init = root.join("scripts/build/rootfs-gateway.sh");
    if !init.is_file() {
        return Err(format!(
            "{} missing -- run the build from the repository checkout",
            init.display()
        ));
    }
    let rbuild = root.join("target/rootfs-build");
    let rootdir = rbuild.join("root");
    if !rootdir.is_dir() {
        println!("cella: rootfs gateway: the canonical tree is absent, building it first");
        rootfs_canonical(canonical_golden)?;
    }
    println!("cella: rootfs gateway: assembling");
    let groot = rbuild.join("root-gateway");
    let _ = fs::remove_dir_all(&groot);
    run(
        "copy the root",
        "cp",
        &["-a", rootdir.to_str().unwrap(), groot.to_str().unwrap()],
        None,
    )?;
    fs::copy(&init, groot.join("sbin/init")).map_err(|e| e.to_string())?;
    let mut perm = fs::metadata(groot.join("sbin/init"))
        .map_err(|e| e.to_string())?
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(groot.join("sbin/init"), perm).map_err(|e| e.to_string())?;

    let img = rbuild.join("rootfs-gateway.ext4");
    let _ = fs::remove_file(&img);
    let f = fs::File::create(&img).map_err(|e| e.to_string())?;
    f.set_len(16 * 1024 * 1024).map_err(|e| e.to_string())?;
    drop(f);
    run_in_toolbox_quiet(
        "mkfs",
        &rbuild,
        &[
            "mkfs.ext4",
            "-q",
            "-F",
            "-d",
            groot.to_str().unwrap(),
            img.to_str().unwrap(),
        ],
    )?;
    fs::create_dir_all(golden.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = golden.with_extension("tmp");
    fs::copy(&img, &tmp).map_err(|e| e.to_string())?;
    fs::rename(&tmp, golden).map_err(|e| e.to_string())?;
    println!("cella: golden rootfs gateway -> {}", golden.display());
    Ok(())
}

/// The nested kernel: the canonical fragment plus the KVM host
/// stack, from the same pinned source, in a copied clean tree. The
/// canonical tree stays as the canonical cache.
pub fn kernel_nested(golden: &Path) -> Result<(), String> {
    let root = repo_root();
    let frag = root.join("scripts/build/kernel-fragment.config");
    let nfrag = root.join("scripts/build/kernel-fragment-nested.config");
    for f in [&frag, &nfrag] {
        if !f.is_file() {
            return Err(format!("{} missing", f.display()));
        }
    }
    let kbuild = root.join("target/kernel-build");
    let src = kbuild.join(format!("linux-{KERNEL_VERSION}"));
    if !src.is_dir() {
        // The canonical build fetches the source; reuse its cache.
        kernel_canonical(&crate::machine::kernel_path("canonical"))?;
    }
    let nsrc = kbuild.join(format!("linux-{KERNEL_VERSION}-nested"));
    if !nsrc.is_dir() {
        println!("cella: kernel nested: copying the source for a clean tree");
        run(
            "copy the source",
            "cp",
            &["-a", src.to_str().unwrap(), nsrc.to_str().unwrap()],
            None,
        )?;
        run_in_toolbox_quiet("mrproper", &nsrc, &["make", "mrproper"])?;
    }
    println!("cella: kernel nested: configuring (defconfig + both fragments)");
    run_in_toolbox_quiet("defconfig", &nsrc, &["make", "x86_64_defconfig"])?;
    run_in_toolbox_quiet(
        "merge",
        &nsrc,
        &[
            "scripts/kconfig/merge_config.sh",
            "-m",
            ".config",
            frag.to_str().unwrap(),
            nfrag.to_str().unwrap(),
        ],
    )?;
    run_in_toolbox_quiet("olddefconfig", &nsrc, &["make", "olddefconfig"])?;
    assert_config(
        &nsrc.join(".config"),
        &[
            "CONFIG_KVM",
            "CONFIG_TUN",
            "CONFIG_USER_NS",
            "CONFIG_PID_NS",
            "CONFIG_SECCOMP_FILTER",
            "CONFIG_DEVTMPFS_MOUNT",
        ],
    )?;
    let cfg = fs::read_to_string(nsrc.join(".config")).map_err(|e| e.to_string())?;
    if !cfg.contains("CONFIG_KVM_INTEL=y") && !cfg.contains("CONFIG_KVM_AMD=y") {
        return Err("no vendor KVM module survived in the nested kernel config".to_string());
    }
    println!("cella: kernel nested: building bzImage");
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .to_string();
    run_in_toolbox_quiet("build", &nsrc, &["make", "-j", &jobs, "bzImage"])?;
    let built = nsrc.join("arch/x86/boot/bzImage");
    fs::create_dir_all(golden.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = golden.with_extension("tmp");
    fs::copy(&built, &tmp).map_err(|e| e.to_string())?;
    fs::rename(&tmp, golden).map_err(|e| e.to_string())?;
    println!("cella: golden kernel nested -> {}", golden.display());
    Ok(())
}

/// The pinned bubblewrap for the in-guest jail. The version moves
/// only deliberately, like the kernel pin.
pub const BWRAP_VERSION: &str = "0.11.0";

/// A static bubblewrap, built in the toolbox: the nested image runs
/// the same jail inside the guest, and the busybox rootfs has no
/// shared libraries.
fn bwrap_static() -> Result<PathBuf, String> {
    let root = repo_root();
    let bbuild = root.join("target/bwrap-build");
    let src = bbuild.join(format!("bubblewrap-{BWRAP_VERSION}"));
    let out = src.join("build/bwrap");
    if out.is_file() {
        return Ok(out);
    }
    let url = format!(
        "https://github.com/containers/bubblewrap/releases/download/v{BWRAP_VERSION}/bubblewrap-{BWRAP_VERSION}.tar.xz"
    );
    let tarball = bbuild.join(format!("bubblewrap-{BWRAP_VERSION}.tar.xz"));
    fetch_and_extract("bwrap", &url, &tarball, &src, &bbuild)?;
    println!("cella: bwrap: configuring (static, no selinux, no man)");
    run_in_toolbox_quiet(
        "meson setup",
        &src,
        &[
            "env",
            "LDFLAGS=-static",
            "meson",
            "setup",
            "build",
            "-Dprefer_static=true",
            "-Dselinux=disabled",
            "-Dman=disabled",
            "-Dtests=false",
            "-Dbash_completion=disabled",
            "-Dzsh_completion=disabled",
        ],
    )?;
    println!("cella: bwrap: building");
    run_in_toolbox_quiet("ninja", &src, &["ninja", "-C", "build"])?;
    let ldd = Command::new("ldd").arg(&out).output();
    if let Ok(o) = ldd {
        let text =
            String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        if !text.contains("not a dynamic executable") && !text.contains("statically linked") {
            return Err("bwrap did not link statically".to_string());
        }
    }
    Ok(out)
}

/// The static binaries for the in-guest layers: cella and
/// cella-probe, crt-static against glibc-static in the toolbox. One
/// cargo invocation builds every bin of the package.
fn build_static_binaries() -> Result<(PathBuf, PathBuf), String> {
    let root = repo_root();
    println!("cella: building the static cella and the static cella-probe");
    let rustflags = "RUSTFLAGS=-C target-feature=+crt-static";
    run_in_toolbox_quiet(
        "static binaries",
        &root,
        &[
            "env",
            rustflags,
            "cargo",
            "build",
            "--release",
            "--target",
            "x86_64-unknown-linux-gnu",
        ],
    )?;
    let bin = root.join("target/x86_64-unknown-linux-gnu/release/cella");
    let probe = root.join("target/x86_64-unknown-linux-gnu/release/cella-probe");
    for p in [&bin, &probe] {
        if !p.is_file() {
            return Err(format!("{} did not build", p.display()));
        }
    }
    Ok((bin, probe))
}

/// One nested-family rootfs: the canonical tree, the static cella on
/// the path, the canonical inner assets at the golden layout of the
/// guest (/root/.cella, the same convention as the host), and the
/// given init. The inception variant adds the static probe.
fn rootfs_nested_family(
    golden: &Path,
    init_name: &str,
    with_probe: bool,
    with_jail: bool,
    tree_name: &str,
) -> Result<(), String> {
    let root = repo_root();
    let init = root.join("scripts/build").join(init_name);
    if !init.is_file() {
        return Err(format!("{} missing", init.display()));
    }
    let rbuild = root.join("target/rootfs-build");
    if !rbuild.join("root").is_dir() {
        rootfs_canonical(&crate::machine::rootfs_path("canonical"))?;
    }
    let inner_kernel = crate::machine::kernel_path("canonical");
    let inner_rootfs = crate::machine::rootfs_path("canonical");
    for p in [&inner_kernel, &inner_rootfs] {
        if !p.is_file() {
            return Err(format!(
                "inner asset {} missing -- build the canonical flavors first",
                p.display()
            ));
        }
    }
    let (bin, probe) = build_static_binaries()?;

    println!("cella: rootfs {tree_name}: assembling");
    let nroot = rbuild.join(tree_name);
    let _ = fs::remove_dir_all(&nroot);
    run(
        "copy the root",
        "cp",
        &[
            "-a",
            rbuild.join("root").to_str().unwrap(),
            nroot.to_str().unwrap(),
        ],
        None,
    )?;
    fs::create_dir_all(nroot.join("root/.cella/kernel/canonical")).map_err(|e| e.to_string())?;
    fs::create_dir_all(nroot.join("root/.cella/rootfs/canonical")).map_err(|e| e.to_string())?;
    // The manifests travel with the goldens: each layer verifies its
    // inherited artifacts (cella doctor verify) before it boots an
    // inner machine. Green-field: a golden without a manifest is a
    // build error, not a heal case.
    let inner_kernel_manifest = crate::golden::manifest_path(&inner_kernel);
    let inner_rootfs_manifest = crate::golden::manifest_path(&inner_rootfs);
    for m in [&inner_kernel_manifest, &inner_rootfs_manifest] {
        if !m.is_file() {
            return Err(format!(
                "{} is absent -- rebuild the golden (cella build) so its manifest exists",
                m.display()
            ));
        }
    }
    let mut copies = vec![
        (bin, nroot.join("bin/cella"), 0o755),
        (
            inner_kernel,
            nroot.join("root/.cella/kernel/canonical/bzImage"),
            0o644,
        ),
        (
            inner_kernel_manifest,
            nroot.join("root/.cella/kernel/canonical/golden.json"),
            0o444,
        ),
        (
            inner_rootfs,
            nroot.join("root/.cella/rootfs/canonical/rootfs.ext4"),
            0o644,
        ),
        (
            inner_rootfs_manifest,
            nroot.join("root/.cella/rootfs/canonical/golden.json"),
            0o444,
        ),
        (init, nroot.join("sbin/init"), 0o755),
    ];
    if with_probe {
        copies.push((probe, nroot.join("bin/cella-probe"), 0o755));
    }
    if with_jail {
        // The in-guest verbs run the same jail as the host.
        copies.push((bwrap_static()?, nroot.join("bin/bwrap"), 0o755));
    }
    for (from, to, mode) in copies {
        fs::copy(&from, &to).map_err(|e| format!("copying {}: {e}", from.display()))?;
        let mut p = fs::metadata(&to).map_err(|e| e.to_string())?.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut p, mode);
        fs::set_permissions(&to, p).map_err(|e| e.to_string())?;
    }
    let img = rbuild.join(format!("{tree_name}.ext4"));
    let _ = fs::remove_file(&img);
    let f = fs::File::create(&img).map_err(|e| e.to_string())?;
    f.set_len(64 * 1024 * 1024).map_err(|e| e.to_string())?;
    drop(f);
    run_in_toolbox_quiet(
        "mkfs",
        &rbuild,
        &[
            "mkfs.ext4",
            "-q",
            "-F",
            "-d",
            nroot.to_str().unwrap(),
            img.to_str().unwrap(),
        ],
    )?;
    fs::create_dir_all(golden.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = golden.with_extension("tmp");
    fs::copy(&img, &tmp).map_err(|e| e.to_string())?;
    fs::rename(&tmp, golden).map_err(|e| e.to_string())?;
    println!("cella: golden rootfs {tree_name} -> {}", golden.display());
    Ok(())
}

pub fn rootfs_nested(golden: &Path) -> Result<(), String> {
    rootfs_nested_family(golden, "rootfs-nested.sh", false, true, "root-nested")
}

pub fn rootfs_inception(golden: &Path) -> Result<(), String> {
    rootfs_nested_family(golden, "rootfs-inception.sh", true, false, "root-inception")
}
