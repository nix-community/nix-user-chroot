//! Integration tests for the path-config.toml feature.
//!
//! These tests spin up an actual user namespace + chroot via the built
//! binary, so they require unprivileged user namespaces to be enabled
//! on the host (same requirement as nix-user-chroot itself).
//!
//! Because the chroot uses a fake empty nixdir, /bin/sh on NixOS (a
//! symlink into /nix/store) won't resolve. We copy a static busybox
//! into the tempdir instead — /tmp is mirrored into the chroot so the
//! copy is reachable at the same path inside.

use nix_user_chroot::mkdtemp;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = env!("TARGET");

/// Locates a static busybox binary. Tries $NIX_USER_CHROOT_TEST_BUSYBOX
/// first, then common system paths, then `nix-build`.
fn find_static_busybox() -> PathBuf {
    if let Ok(p) = std::env::var("NIX_USER_CHROOT_TEST_BUSYBOX") {
        return PathBuf::from(p);
    }
    for p in ["/bin/busybox", "/usr/bin/busybox"] {
        if Path::new(p).exists() {
            return PathBuf::from(p);
        }
    }
    // Fall back to building a static busybox via nix.
    let out = Command::new("nix-build")
        .args(["<nixpkgs>", "-A", "pkgsStatic.busybox", "--no-out-link"])
        .output()
        .expect("nix-build not available and no busybox found");
    assert!(
        out.status.success(),
        "nix-build pkgsStatic.busybox failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let store_path = String::from_utf8(out.stdout).unwrap();
    PathBuf::from(store_path.trim()).join("bin/busybox")
}

struct TestEnv {
    root: PathBuf,
    nixdir: PathBuf,
    sh: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let root = mkdtemp::mkdtemp("/tmp/nix-user-chroot-test.XXXXXX").unwrap();
        let nixdir = root.join("nix");
        fs::create_dir_all(&nixdir).unwrap();

        // Copy a static busybox into the tempdir so it's reachable inside the
        // chroot via the / mirror pass, and create applet symlinks for the
        // commands the test scripts use.
        let busybox_src = find_static_busybox();
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        let busybox = bin.join("busybox");
        fs::copy(&busybox_src, &busybox).unwrap_or_else(|e| {
            panic!(
                "failed to copy {} -> {}: {e}",
                busybox_src.display(),
                busybox.display()
            )
        });
        fs::set_permissions(&busybox, fs::Permissions::from_mode(0o755)).unwrap();
        for applet in ["sh", "cat", "test", "echo"] {
            symlink("busybox", bin.join(applet)).unwrap();
        }
        let sh = bin.join("sh");

        TestEnv { root, nixdir, sh }
    }

    fn write_config(&self, toml: &str) {
        let cfg_dir = self.nixdir.join("etc/nix-user-chroot");
        fs::create_dir_all(&cfg_dir).unwrap();
        fs::write(cfg_dir.join("path-config.toml"), toml).unwrap();
    }

    /// Runs nix-user-chroot with a shell snippet, returns (success, stdout, stderr).
    /// PATH is set to the tempdir's bin so busybox applets (cat, test, ...) resolve.
    fn run(&self, script: &str) -> (bool, String, String) {
        let bin = self.sh.parent().unwrap();
        let out = Command::new("cargo")
            .args([
                "run",
                "--quiet",
                "--target",
                TARGET,
                "--",
                self.nixdir.to_str().unwrap(),
                self.sh.to_str().unwrap(),
                "-c",
                &format!("PATH={}; {script}", bin.display()),
            ])
            .output()
            .expect("failed to spawn cargo run");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn no_config_baseline() {
    let env = TestEnv::new();
    let (ok, stdout, stderr) = env.run("test -d /nix && echo ok");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "ok");
}

#[test]
fn absolute_mount() {
    let env = TestEnv::new();
    let src = env.root.join("my-marker");
    fs::write(&src, "hello-from-absolute-mount\n").unwrap();

    env.write_config(&format!(
        r#"
[absolute]
"{}" = "/nuct-mnt/marker"
"#,
        src.display()
    ));

    let (ok, stdout, stderr) = env.run("cat /nuct-mnt/marker");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "hello-from-absolute-mount");
}

#[test]
fn exclude_path() {
    let env = TestEnv::new();

    // Create a file we control under /tmp (mirrored into the chroot) so we
    // can exclude it without triggering recursion into host dirs like /etc
    // that may contain /nix/store symlinks our fake nixdir can't resolve.
    let target = env.root.join("to-be-excluded");
    fs::write(&target, "should-not-be-visible\n").unwrap();
    let target = target.to_str().unwrap();

    env.write_config(&format!(
        r#"
[excludes]
paths = ["{target}"]
"#
    ));

    // After exclusion the /dev/null placeholder is unmounted, leaving an
    // empty regular file. `test -s` (nonzero size) should fail.
    let (ok, stdout, stderr) = env.run(&format!(
        "if test -s {target}; then echo present; else echo excluded; fi"
    ));
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "excluded", "stderr: {stderr}");
}

#[test]
fn profile_mount() {
    let env = TestEnv::new();

    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .unwrap()
        .expect("current uid has no passwd entry")
        .name;

    // Build the profile symlink chain nix-user-chroot expects:
    //   <nixdir>/var/nix/profiles/per-user/<user>/profile -> profile-1
    //   profile-1/bin/marker  (a real file)
    let profiles = env.nixdir.join("var/nix/profiles/per-user").join(&user);
    fs::create_dir_all(&profiles).unwrap();
    let gen = profiles.join("profile-1");
    fs::create_dir_all(gen.join("bin")).unwrap();
    fs::write(gen.join("bin/marker"), "hello-from-profile\n").unwrap();
    symlink("profile-1", profiles.join("profile")).unwrap();

    env.write_config(
        r#"
[profile]
"bin/marker" = "/nuct-mnt/profile-marker"
"#,
    );

    let (ok, stdout, stderr) = env.run("cat /nuct-mnt/profile-marker");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "hello-from-profile");
}

#[test]
fn config_parse_error() {
    let env = TestEnv::new();
    env.write_config("this is = not [ valid toml");

    let (ok, _stdout, stderr) = env.run("true");
    assert!(!ok, "expected failure on bad config, got stderr: {stderr}");
    assert!(
        stderr.contains("failed to parse config file"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("path-config.toml"),
        "error should mention the file path, got: {stderr}"
    );
}

#[test]
fn exclude_must_be_absolute() {
    let env = TestEnv::new();
    env.write_config(
        r#"
[excludes]
paths = ["relative/path"]
"#,
    );

    let (ok, _stdout, stderr) = env.run("true");
    assert!(
        !ok,
        "expected failure on relative exclude, got stderr: {stderr}"
    );
    assert!(stderr.contains("must be absolute"), "stderr: {stderr}");
}

#[test]
fn optional_sections() {
    let env = TestEnv::new();

    // Only [absolute], no [excludes] or [profile].
    let src = env.root.join("only-abs");
    fs::write(&src, "partial-config\n").unwrap();
    env.write_config(&format!(
        r#"
[absolute]
"{}" = "/nuct-mnt/only-abs"
"#,
        src.display()
    ));

    let (ok, stdout, stderr) = env.run("cat /nuct-mnt/only-abs");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "partial-config");
}
