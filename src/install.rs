//! `--install` bootstrap: fetch the NixOS nix-installer and run it inside
//! our user namespace so it sees a writable `/nix` without real root.

use std::{
    env, fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process,
};

const INSTALLER_BASE_URL: &str = "https://artifacts.nixos.org/nix-installer";

/// Determine the default nix store location when the user didn't pass one.
///
/// Follows XDG: `$XDG_DATA_HOME/nix`, falling back to `~/.local/share/nix`.
pub fn default_nixpath() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .expect("neither XDG_DATA_HOME nor HOME is set")
        .join("nix")
}

fn installer_arch() -> &'static str {
    match env::consts::ARCH {
        "x86_64" => "x86_64-linux",
        "aarch64" => "aarch64-linux",
        other => {
            eprintln!("--install: unsupported architecture {other}");
            eprintln!("nix-installer ships binaries for x86_64 and aarch64 only.");
            process::exit(1);
        }
    }
}

/// Fetch the nix-installer binary to a temp file and return its path.
///
/// Shells out to `curl` to avoid bundling an HTTP+TLS stack into a binary
/// whose normal operation never touches the network. Honours
/// `NIX_USER_CHROOT_INSTALLER` to skip the download (useful for offline
/// bootstrap and for the integration tests).
pub fn fetch_installer() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("NIX_USER_CHROOT_INSTALLER") {
        let path = PathBuf::from(path);
        if !path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("NIX_USER_CHROOT_INSTALLER points at {path:?} which does not exist"),
            ));
        }
        return Ok(path);
    }

    let url = format!("{INSTALLER_BASE_URL}/nix-installer-{}", installer_arch());
    let dest = env::temp_dir().join(format!("nix-installer.{}", process::id()));

    eprintln!("Fetching {url}");
    let status = process::Command::new("curl")
        .args(["-sSfL", "--output"])
        .arg(&dest)
        .arg(&url)
        .status()
        .map_err(|e| io::Error::other(format!("failed to spawn curl (is it installed?): {e}")))?;

    if !status.success() {
        let _ = fs::remove_file(&dest);
        return Err(io::Error::other(format!(
            "curl exited with {status} while fetching {url}"
        )));
    }

    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
    Ok(dest)
}

/// Command line to run inside the chroot.
///
/// `--rootless` keeps the installer entirely under `/nix`: no nixbld
/// users/group, no `/etc/nix/nix.conf`, no shell-profile edits, no init
/// integration. All of those would fail anyway since our chroot bind-mounts
/// the host `/etc` read-only and the user namespace only has a single uid/gid
/// mapped. The install receipt lands in `/nix/receipt.json` which surfaces on
/// the host as `<nixpath>/receipt.json` for later `uninstall`.
pub fn installer_command(installer: &Path) -> (String, Vec<String>) {
    (
        installer.to_string_lossy().into_owned(),
        vec![
            "install".into(),
            "linux".into(),
            "--rootless".into(),
            "--no-confirm".into(),
        ],
    )
}

pub fn print_next_steps(nixpath: &Path) {
    eprintln!();
    eprintln!("Installation complete. Enter the environment with:");
    eprintln!();
    eprintln!("    nix-user-chroot {} bash -l", nixpath.display());
    eprintln!();
}
