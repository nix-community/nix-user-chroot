use nix_user_chroot::mkdtemp;
use std::env;
use std::fs;
use std::process::Command;

const TARGET: &str = env!("TARGET");

#[test]
fn run_nix_install() {
    let tempdir = mkdtemp::mkdtemp("/tmp/nix.XXXXXX").unwrap();

    let result = Command::new("cargo")
        .args(&[
            "run",
            "--target",
            TARGET,
            tempdir.to_str().unwrap(),
            "bash",
            "-c",
            "curl https://nixos.org/nix/install | bash",
        ])
        .status();
    fs::remove_dir_all(tempdir).unwrap();
    assert!(result.unwrap().success());
}
