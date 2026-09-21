//! The build verb's dispatch and the golden manifests: moved from
//! the machine module at the split (1.6.13) -- build machinery
//! belongs to the build persona, and the doctor's fix is its one
//! other user.

use std::fs;
use std::path::{Path, PathBuf};

use cella_libs::machine::{kernel_path, manifest_field, rootfs_path};

/// recipe; an unknown pair is an error, not a fallback.
pub fn build(axis: &str, flavor: &str) -> Result<(), String> {
    build_flags(axis, flavor, false)
}

/// A golden that exists is done: the artifacts are canonical, and a
/// rebuild produces the same one at real cost (see
/// docs/LIFECYCLE.md). `--fresh` rebuilds deliberately.
pub fn build_flags(axis: &str, flavor: &str, fresh: bool) -> Result<(), String> {
    let out = match axis {
        "kernel" => kernel_path(flavor),
        "rootfs" => rootfs_path(flavor),
        _ => PathBuf::new(),
    };
    if !fresh && out.is_file() {
        // Skip only when the recorded inputs still match: a changed
        // init script or fragment makes the golden stale, and a
        // stale golden lies quietly (a pegasus inspect booted an
        // image whose init predated /rock). The manifest carries
        // the input digests; compare them.
        match stale_inputs(axis, flavor, &out) {
            Ok(None) => {
                println!(
                    "cella: {axis} {flavor} already built at {} (inputs unchanged; --fresh rebuilds)",
                    out.display()
                );
                return Ok(());
            }
            Ok(Some(input)) => {
                println!(
                    "cella: {axis} {flavor}: input {input} changed since the build -- rebuilding"
                );
            }
            Err(e) => {
                println!("cella: {axis} {flavor}: {e} -- rebuilding");
            }
        }
    }
    match (axis, flavor) {
        ("kernel", "canonical") => crate::orchestrate::kernel_canonical(&kernel_path(flavor)),
        ("kernel", "nested") => crate::orchestrate::kernel_nested(&kernel_path(flavor)),
        ("rootfs", "canonical") => crate::orchestrate::rootfs_canonical(&rootfs_path(flavor)),
        ("rootfs", "cella") => {
            crate::orchestrate::rootfs_cella(&rootfs_path(flavor), &rootfs_path("canonical"))
        }
        ("rootfs", "gateway") => {
            crate::orchestrate::rootfs_gateway(&rootfs_path(flavor), &rootfs_path("canonical"))
        }
        ("rootfs", "terminator") => {
            crate::orchestrate::rootfs_terminator(&rootfs_path(flavor), &rootfs_path("canonical"))
        }
        ("rootfs", "nested") => crate::orchestrate::rootfs_nested(&rootfs_path(flavor)),
        ("rootfs", "inception") => crate::orchestrate::rootfs_inception(&rootfs_path(flavor)),
        _ => Err(format!(
            "unknown build target {axis:?} {flavor:?} -- axes: kernel, rootfs; see docs/LIFECYCLE.md"
        )),
    }?;
    write_golden_manifest(axis, flavor, &out)
}

/// The pinned inputs for one artifact: the scripts and fragments
/// that shape it -- and for the terminator, the proxy's own source
/// tree, because the image bakes the binary and a code change must
/// rebake without a manual manifest purge (the 2026-09-20 lockout
/// hunt spent an afternoon testing a stale guest binary).
fn build_inputs(axis: &str, flavor: &str, artifact_dir: &Path) -> Vec<std::path::PathBuf> {
    let root = crate::orchestrate::repo_root();
    let b = root.join("scripts/build");
    match axis {
        "kernel" => vec![
            b.join("kernel-fragment.config"),
            b.join("kernel-fragment-nested.config"),
        ],
        _ => {
            let mut v = vec![
                b.join(format!("rootfs-{flavor}.sh")),
                b.join("rootfs.sh"),
                b.join("busybox-fragment.config"),
            ];
            if flavor == "terminator" {
                // The pair's identity: a changed cert is a changed
                // pair, and the manifest must say so.
                v.push(artifact_dir.join("ca.pem"));
                let src = root.join("crates/cella-terminator");
                v.push(src.join("Cargo.toml"));
                let mut rs: Vec<_> = std::fs::read_dir(src.join("src"))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "rs"))
                    .collect();
                rs.sort();
                v.extend(rs);
            }
            v
        }
    }
}

/// Stale check: the name of the first build input whose digest no
/// longer matches the manifest, None when everything matches. A
/// missing or unreadable manifest is an error (rebuild).
fn stale_inputs(axis: &str, flavor: &str, out: &Path) -> Result<Option<String>, String> {
    let mpath = cella_libs::golden::manifest_path(out);
    let text =
        fs::read_to_string(&mpath).map_err(|_| format!("no manifest at {}", mpath.display()))?;
    let inputs = build_inputs(axis, flavor, out.parent().unwrap());
    for input in inputs {
        if !input.is_file() {
            continue;
        }
        let name = input.file_name().unwrap().to_string_lossy().to_string();
        let recorded = manifest_field(&text, &format!("input_{name}"));
        let actual = cella_libs::golden::sha3_256_hex(&input)?;
        if recorded.as_deref() != Some(actual.as_str()) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// The manifest, after every successful build, in one place. The
/// build inputs pinned per axis: the kernel fragments for a kernel,
/// the init script and the busybox fragment for a rootfs.
fn write_golden_manifest(axis: &str, flavor: &str, artifact: &Path) -> Result<(), String> {
    let sources: Vec<(&str, &str)> = vec![
        ("kernel", crate::orchestrate::KERNEL_VERSION),
        ("busybox", crate::orchestrate::BUSYBOX_VERSION),
        ("bash", crate::orchestrate::GUEST_BASH_VERSION),
    ];
    let inputs = build_inputs(axis, flavor, artifact.parent().unwrap());
    let input_refs: Vec<&Path> = inputs.iter().map(|p| p.as_path()).collect();
    cella_libs::golden::write_manifest(artifact, axis, flavor, &sources, &input_refs)?;
    println!(
        "cella: wrote {} (sha3-256, read-only)",
        cella_libs::golden::manifest_path(artifact).display()
    );
    Ok(())
}
