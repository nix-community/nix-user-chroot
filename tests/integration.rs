use std::env;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn run_nix_install() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let cmd_path = root.join("target/debug/nix-user-chroot");
    assert!(cmd_path.exists());

    let tempdir = TempDir::new().unwrap();

    let result = Command::new(cmd_path)
        .args(&[
            tempdir.path().to_str().unwrap(),
            "bash",
            "-c",
            "curl https://nixos.org/nix/install | bash",
        ])
        .status()
        .unwrap();
    assert!(result.success());
}
