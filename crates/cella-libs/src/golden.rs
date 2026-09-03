//! Golden manifests: the record of what a build produced.
//!
//! `cella build` writes one `golden.json` beside each artifact: the
//! sha3-256 of the artifact, the source versions, and the digests of
//! the build inputs that shaped it. The file is read-only (mode 444):
//! the manifest states what was built, and nothing edits that
//! statement. Verification lives in doctor, not here and not in
//! build -- build makes, doctor judges. This module is the seed of
//! cella-libs (see tasks/PHASE1-core.md).

use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use sha3::{Digest, Sha3_256};

/// The sha3-256 of a file, streamed, as lowercase hex.
pub fn sha3_256_hex(path: &Path) -> Result<String, String> {
    let mut f =
        File::open(path).map_err(|e| format!("open {} for hashing: {e}", path.display()))?;
    let mut hasher = Sha3_256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {} for hashing: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The manifest path for an artifact: `golden.json` in its directory.
pub fn manifest_path(artifact: &Path) -> std::path::PathBuf {
    artifact
        .parent()
        .expect("artifact path has a parent")
        .join("golden.json")
}

/// Write the manifest beside the artifact. `inputs` are the build
/// inputs worth pinning (config fragments, init scripts), each hashed;
/// an input file that does not exist is skipped, not an error. The
/// file lands read-only; an existing one is replaced.
pub fn write_manifest(
    artifact: &Path,
    axis: &str,
    flavor: &str,
    sources: &[(&str, &str)],
    inputs: &[&Path],
) -> Result<(), String> {
    let digest = sha3_256_hex(artifact)?;
    let bytes = fs::metadata(artifact)
        .map_err(|e| format!("stat {}: {e}", artifact.display()))?
        .len();
    let built = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut s = String::from("{\n");
    s.push_str(&format!("  \"axis\": \"{axis}\",\n"));
    s.push_str(&format!("  \"flavor\": \"{flavor}\",\n"));
    s.push_str(&format!(
        "  \"artifact\": \"{}\",\n",
        artifact.file_name().unwrap().to_string_lossy()
    ));
    s.push_str(&format!("  \"sha3_256\": \"{digest}\",\n"));
    s.push_str(&format!("  \"bytes\": {bytes},\n"));
    s.push_str(&format!("  \"built_epoch\": {built},\n"));
    for (name, version) in sources {
        s.push_str(&format!("  \"source_{name}\": \"{version}\",\n"));
    }
    for input in inputs {
        if !input.is_file() {
            continue;
        }
        let h = sha3_256_hex(input)?;
        s.push_str(&format!(
            "  \"input_{}\": \"{h}\",\n",
            input.file_name().unwrap().to_string_lossy()
        ));
    }
    // The last line carries no comma: hand-rolled JSON, same policy
    // as the machine manifest (no serialization dependency).
    s.truncate(s.trim_end_matches(",\n").len());
    s.push_str("\n}\n");

    let path = manifest_path(artifact);
    if path.exists() {
        // Mode 444 blocks the rewrite; lift it first.
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o644));
    }
    fs::write(&path, s).map_err(|e| format!("write {}: {e}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444))
        .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    Ok(())
}

/// One field of the flat manifest object, or None.
pub fn field(manifest_text: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":");
    let i = manifest_text.find(&pat)?;
    let rest = manifest_text[i + pat.len()..].trim_start();
    if let Some(r) = rest.strip_prefix('"') {
        r.split('"').next().map(str::to_string)
    } else {
        rest.split(|c: char| c == ',' || c == '}' || c.is_whitespace())
            .next()
            .map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cella-golden-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// The digest matches an independent computation, and the
    /// manifest round-trips its fields.
    #[test]
    fn manifest_records_the_artifact_digest() {
        let d = tmp("digest");
        let artifact = d.join("bzImage");
        fs::write(&artifact, b"not a kernel").unwrap();
        let input = d.join("fragment.config");
        fs::write(&input, b"CONFIG_TEST=y\n").unwrap();

        write_manifest(
            &artifact,
            "kernel",
            "canonical",
            &[("kernel", "7.2.2")],
            &[&input, &d.join("absent.config")],
        )
        .unwrap();

        let text = fs::read_to_string(manifest_path(&artifact)).unwrap();
        assert_eq!(field(&text, "axis").as_deref(), Some("kernel"));
        assert_eq!(field(&text, "flavor").as_deref(), Some("canonical"));
        assert_eq!(field(&text, "source_kernel").as_deref(), Some("7.2.2"));
        assert_eq!(
            field(&text, "sha3_256").as_deref(),
            Some(sha3_256_hex(&artifact).unwrap().as_str())
        );
        assert!(field(&text, "input_fragment.config").is_some());
        assert!(field(&text, "input_absent.config").is_none());

        // Read-only: the manifest states, nothing edits.
        let mode = fs::metadata(manifest_path(&artifact))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o444);

        // A rebuild replaces it without error.
        write_manifest(&artifact, "kernel", "canonical", &[], &[]).unwrap();

        let _ = fs::remove_dir_all(&d);
    }

    /// A changed artifact changes the digest: the fact doctor verify
    /// will stand on.
    #[test]
    fn digest_tracks_the_artifact() {
        let d = tmp("tracks");
        let artifact = d.join("rootfs.ext4");
        fs::write(&artifact, b"aaa").unwrap();
        let h1 = sha3_256_hex(&artifact).unwrap();
        fs::write(&artifact, b"aab").unwrap();
        let h2 = sha3_256_hex(&artifact).unwrap();
        assert_ne!(h1, h2);
        assert_eq!(h1.len(), 64);
        let _ = fs::remove_dir_all(&d);
    }
}
