use nix_user_chroot::mkdtemp;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

const TARGET: &str = env!("TARGET");

/// Returns a path to a static busybox that is reachable inside the chroot.
///
/// On NixOS the busybox provided by the dev shell lives under /nix/store,
/// which is replaced with an empty directory inside the chroot. Copy it to
/// /tmp so the same path resolves on both sides. The copy must be named
/// "busybox" because busybox dispatches applets by argv[0] basename.
fn busybox_in_tmp() -> String {
    let src = env::var("NIX_USER_CHROOT_TEST_BUSYBOX")
        .expect("NIX_USER_CHROOT_TEST_BUSYBOX must point to a static busybox binary");
    let dir = mkdtemp::mkdtemp("/tmp/nix-user-chroot-test.XXXXXX").unwrap();
    let dst = dir.join("busybox");
    fs::copy(&src, &dst)
        .unwrap_or_else(|e| panic!("failed to copy {} -> {}: {}", src, dst.display(), e));
    dst.to_str().unwrap().to_string()
}

/// Smoke test that works on any host (including NixOS): verify we can
/// pivot_root into the chroot, see the host filesystem, and that /nix
/// points at our empty nixdir rather than the host's store.
#[test]
fn run_chroot_smoke() {
    let nixdir = mkdtemp::mkdtemp("/tmp/nix.XXXXXX").unwrap();
    let busybox = busybox_in_tmp();

    let out = Command::new("cargo")
        .args([
            "run",
            "--target",
            TARGET,
            nixdir.to_str().unwrap(),
            &busybox,
            "sh",
            "-c",
            // /tmp from the host must be visible, /nix must be empty
            &format!(
                "test -x {bb} && test -d /nix && test -z \"$({bb} ls -A /nix)\" && echo ok",
                bb = busybox
            ),
        ])
        .output()
        .expect("failed to spawn cargo run");

    fs::remove_dir_all(&nixdir).unwrap();

    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
}

/// Full end-to-end test that runs the upstream Nix installer.
///
/// Skipped on NixOS because the installer expects a traditional FHS
/// userland (bash, curl, xz, etc.) that is not available once /nix is
/// replaced with an empty directory.
#[test]
fn run_nix_install() {
    if Path::new("/etc/NIXOS").exists() {
        eprintln!("skipping run_nix_install on NixOS (installer needs FHS userland)");
        return;
    }

    let nixdir = mkdtemp::mkdtemp("/tmp/nix.XXXXXX").unwrap();

    let result = Command::new("cargo")
        .args([
            "run",
            "--target",
            TARGET,
            nixdir.to_str().unwrap(),
            "bash",
            "-c",
            "curl https://nixos.org/nix/install | bash",
        ])
        .status();
    fs::remove_dir_all(nixdir).unwrap();
    assert!(result.unwrap().success());
}
