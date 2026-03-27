use std::env;
use std::path::PathBuf;

/// Thin wrapper around `nix::unistd::mkdtemp` that prepends $TMPDIR
/// when the template is relative.
pub fn mkdtemp(template: &str) -> nix::Result<PathBuf> {
    let template = if template.starts_with('/') {
        PathBuf::from(template)
    } else {
        env::temp_dir().join(template)
    };
    nix::unistd::mkdtemp(&template)
}
