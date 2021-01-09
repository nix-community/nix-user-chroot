use std::env;
use std::process::Command;
use tempfile::TempDir;

const TARGET: &str = env!("TARGET");

#[test]
fn run_nix_install() {
    let tempdir = TempDir::new().unwrap();

    let result = Command::new("cargo")
        .args(&[
            "run",
            "--target",
            TARGET,
            tempdir.path().to_str().unwrap(),
            "bash",
            "-c",
            "curl https://nixos.org/nix/install | bash",
        ])
        .status()
        .unwrap();
    assert!(result.success());
}
