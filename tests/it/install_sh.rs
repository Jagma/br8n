use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn target() -> Option<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Some("aarch64-apple-darwin"),
        ("x86_64", "linux") => Some("x86_64-unknown-linux-gnu"),
        ("aarch64", "linux") => Some("aarch64-unknown-linux-gnu"),
        _ => None,
    }
}

fn release_dir(dir: &Path, record: &Path, corrupt: bool) -> String {
    use sha2::Digest;
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> \"{}\"\nexit 0\n",
        record.display()
    );
    let mut header = tar::Header::new_gnu();
    header.set_size(script.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    let mut tgz = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut tgz, flate2::Compression::default());
        let mut ar = tar::Builder::new(enc);
        ar.append_data(&mut header, "br8n", script.as_bytes())
            .unwrap();
        ar.into_inner().unwrap().finish().unwrap();
    }
    let t = target().unwrap();
    std::fs::write(dir.join(format!("br8n-{t}.tar.gz")), &tgz).unwrap();
    let digest = if corrupt {
        "0".repeat(64)
    } else {
        hex::encode(sha2::Sha256::digest(&tgz))
    };
    std::fs::write(
        dir.join(format!("br8n-{t}.sha256")),
        format!("{digest}  br8n-{t}.tar.gz\n"),
    )
    .unwrap();
    format!("file://{}", dir.display())
}

#[test]
fn install_sh_is_valid_shell() {
    let out = Command::new("sh")
        .args(["-n", "scripts/install.sh"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn install_sh_downloads_verifies_and_runs_br8n_install_with_the_flags() {
    let Some(_) = target() else { return };
    let t = tempfile::tempdir().unwrap();
    let record = t.path().join("record");
    let base = release_dir(t.path(), &record, false);
    let out = Command::new("sh")
        .args(["scripts/install.sh", "--yes", "--quiet"])
        .env("BR8N_RELEASE_BASE", &base)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&record).unwrap().trim(),
        "install --yes --quiet"
    );
    let mode = std::fs::metadata("scripts/install.sh")
        .unwrap()
        .permissions()
        .mode();
    assert!(mode & 0o111 != 0, "install.sh must be executable in git");
}

#[test]
fn install_sh_refuses_a_bad_checksum_and_runs_nothing() {
    let Some(_) = target() else { return };
    let t = tempfile::tempdir().unwrap();
    let record = t.path().join("record");
    let base = release_dir(t.path(), &record, true);
    let out = Command::new("sh")
        .args(["scripts/install.sh"])
        .env("BR8N_RELEASE_BASE", &base)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("checksum"));
    assert!(!record.exists());
}
