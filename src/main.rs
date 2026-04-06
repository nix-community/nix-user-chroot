use std::{
    borrow::ToOwned,
    collections::{HashMap, HashSet},
    env,
    ffi::{OsStr, OsString},
    fs::{self, DirEntry},
    io::{self, Write},
    os::unix::{fs::symlink, process::CommandExt},
    path::{Path, PathBuf},
    process,
};

use nix::{
    mount::{mount, umount, MsFlags},
    sched::{unshare, CloneFlags},
    sys::signal::{kill, Signal},
    sys::wait::{waitpid, WaitPidFlag, WaitStatus},
    unistd::{self, fork, ForkResult},
};
use serde::Deserialize;

mod install;
mod mkdtemp;

const NONE: Option<&'static [u8]> = None;

fn bind_mount(source: &Path, dest: &Path) {
    if let Err(e) = mount(
        Some(source),
        dest,
        Some("none"),
        MsFlags::MS_BIND | MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        NONE,
    ) {
        log::error!(
            "failed to bind mount {} to {}: {}",
            source.display(),
            dest.display(),
            e
        );
    }
}

/// When constructing the chroot, the mounts we make either mirror the
/// directory structure in `/` or are explicit user provided mounts that do
/// not necessarily have a source location that mirrors the mount location
/// (i.e. `/home/foo/my/special/groups` -> `/etc/group`).
///
/// We represent the former as [`DirEntry`]s and the latter as regular
/// [`Path`]s.
///
/// We do this instead of just passing around [`PathBuf`]s because `DirEntry`s
/// (which we get when we iterate over `/` recursively as part of mirroring it)
/// have additional guarantees like `file_name` being infallible.
#[derive(Debug, Clone, Copy)]
pub enum DirEntryOrExplicitMount<'a> {
    /// Assumed to share it's directory with the `rootdir` of the [`RunChroot`]
    /// its used with (this is not enforced, however).
    DirEntry(&'a DirEntry),
    /// Assumed to have a different directory than the `rootdir` of the
    /// [`RunChroot`] it's used with.
    ///
    /// For example to mount `/home/foo/bar` to `/bin/bar` you would pass an
    /// `ExplicitMount("/home/foo/bar")` to a `RunChroot` with `rootdir`
    /// `"/bin"`.
    ///
    /// This path *can* be `/`.
    ExplicitMount {
        src: &'a Path,
        dst_file_name: &'a OsStr,
    },
}

impl<'a> From<&'a DirEntry> for DirEntryOrExplicitMount<'a> {
    fn from(de: &'a DirEntry) -> DirEntryOrExplicitMount<'a> {
        DirEntryOrExplicitMount::DirEntry(de)
    }
}

impl<'a> DirEntryOrExplicitMount<'a> {
    fn explicit_mount_with_dest_file_name(
        mount: &'a Path,
        dst_file: &'a (impl AsRef<Path> + 'a),
    ) -> Self {
        let dst_file = dst_file.as_ref();
        let dst_file_name = dst_file.file_name().unwrap_or_else(|| {
            panic!(
                "explicit mount destination `{}` has no file name component \
                 (must not be `/` or end in `..`)",
                dst_file.display()
            )
        });
        DirEntryOrExplicitMount::ExplicitMount {
            src: mount,
            dst_file_name,
        }
    }
}

impl DirEntryOrExplicitMount<'_> {
    fn file_name(&self) -> OsString {
        use DirEntryOrExplicitMount::*;

        match self {
            DirEntry(d) => d.file_name(),
            ExplicitMount { dst_file_name, .. } => (*dst_file_name).to_owned(),
        }
    }

    fn path(&self) -> PathBuf {
        use DirEntryOrExplicitMount::*;

        match self {
            DirEntry(d) => d.path(),
            ExplicitMount { src, .. } => (*src).to_owned(),
        }
    }

    fn metadata(&self) -> io::Result<fs::Metadata> {
        use DirEntryOrExplicitMount::*;

        match self {
            DirEntry(d) => d.metadata(),
            ExplicitMount { src, .. } => src.symlink_metadata(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct PathConfig {
    excludes: ExcludePaths,
    profile: HashMap<PathBuf, PathBuf>,
    absolute: HashMap<PathBuf, PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ExcludePaths {
    paths: HashSet<PathBuf>,
}

pub struct RunChroot<'a> {
    rootdir: &'a Path,
    nixdir: &'a Path,
}

impl<'a> RunChroot<'a> {
    fn new(rootdir: &'a Path, nixdir: &'a Path) -> Self {
        Self { rootdir, nixdir }
    }

    fn with_rootdir(&self, rootdir: &'a Path) -> Self {
        Self {
            rootdir,
            nixdir: self.nixdir,
        }
    }

    /// Recursively resolves a symlink, replacing references to `/nix` with
    /// `self.nixdir` as it goes.
    ///
    /// `stop_at_first_non_nix_path` stops when it sees a path (symlink or not)
    /// that isn't in `/nix`. This exists for [`mirror_symlink`] which
    /// intentionally does not resolve symlinks all the way down when mirroring
    /// them into the chroot.
    fn resolve_nix_path(
        &self,
        p: PathBuf,
        stop_at_first_non_nix_path: bool,
    ) -> io::Result<PathBuf> {
        self.resolve_nix_path_inner(p, stop_at_first_non_nix_path, 0)
    }

    fn resolve_nix_path_inner(
        &self,
        p: PathBuf,
        stop_at_first_non_nix_path: bool,
        depth: u32,
    ) -> io::Result<PathBuf> {
        // Same limit the Linux kernel uses for ELOOP.
        const MAX_SYMLINK_DEPTH: u32 = 40;
        if depth > MAX_SYMLINK_DEPTH {
            return Err(io::Error::other(format!(
                "too many levels of symbolic links resolving {}",
                p.display()
            )));
        }

        if p.is_symlink() {
            let mut target = fs::read_link(&p)?;
            if !target.is_absolute() {
                // need to resolve relative symlinks:
                target = p.parent().unwrap().join(target);
            }

            // replace `/nix` with the actual profile path:
            let p = if let Ok(rest) = target.strip_prefix("/nix") {
                self.nixdir.join(rest)
            } else {
                if stop_at_first_non_nix_path {
                    return Ok(target);
                }

                target
            };

            self.resolve_nix_path_inner(p, stop_at_first_non_nix_path, depth + 1)
        } else if p.exists() {
            Ok(p)
        } else {
            // `p` doesn't exist as-is, but one of its intermediate components
            // may be a symlink into `/nix` that we haven't rewritten yet.
            // Walk from the root onwards, resolving the first such symlink we
            // find, then re-append the remaining components and retry.
            let mut prefix = PathBuf::new();
            let mut components = p.components();

            while let Some(c) = components.next() {
                prefix.push(c);

                if prefix.is_symlink()
                    && prefix
                        .read_link()
                        .map(|t| t.starts_with("/nix"))
                        .unwrap_or(false)
                {
                    let actual_parent =
                        self.resolve_nix_path_inner(prefix, stop_at_first_non_nix_path, depth + 1)?;

                    // re-append the components we haven't consumed yet:
                    let rest: PathBuf = components.collect();
                    let path = actual_parent.join(rest);

                    return self.resolve_nix_path_inner(
                        path,
                        stop_at_first_non_nix_path,
                        depth + 1,
                    );
                }
            }

            Err(io::ErrorKind::NotFound.into())
        }
    }

    // We assume `entry` exists and is actually a directory (not a file or symlink),
    fn bind_mount_directory<'p>(&self, entry: impl Into<DirEntryOrExplicitMount<'p>>) {
        let entry = entry.into();
        let mountpoint = self.rootdir.join(entry.file_name());

        // if the destination doesn't exist we can proceed as normal
        if !mountpoint.exists() {
            if let Err(e) = fs::create_dir(&mountpoint) {
                if e.kind() != io::ErrorKind::AlreadyExists {
                    panic!("failed to create {}: {}", &mountpoint.display(), e);
                }
            }

            log::info!(
                "BIND DIRECTORY {} -> {}",
                entry.path().display(),
                mountpoint.display()
            );

            bind_mount(&entry.path(), &mountpoint)
        } else {
            // otherwise, if the dest is also a dir, we can recurse into it
            // and mount subdirectory siblings of existing paths
            if mountpoint.is_dir() {
                let dir = match fs::read_dir(entry.path()) {
                    Ok(dir) => dir,
                    Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                        log::warn!(
                            "don't have permission to access directory {}, skipping...",
                            entry.path().display()
                        );
                        return;
                    }
                    Err(err) => panic!("failed to list dir {}: {}", entry.path().display(), err),
                };

                let child = self.with_rootdir(&mountpoint);
                for entry in dir {
                    let entry = entry.expect("error while listing subdir");
                    child.bind_mount_entry(&entry);
                }
            }
        }
    }

    // We assume `entry` exists and is actually a file (not a directory or symlink).
    fn bind_mount_file<'p>(&self, entry: impl Into<DirEntryOrExplicitMount<'p>>) {
        let entry = entry.into();
        let mountpoint = self.rootdir.join(entry.file_name());
        log::info!(
            "BIND FILE {} -> {}",
            entry.path().display(),
            mountpoint.display()
        );
        if mountpoint.exists() {
            return;
        }
        fs::File::create(&mountpoint)
            .unwrap_or_else(|err| panic!("failed to create {}: {}", &mountpoint.display(), err));

        bind_mount(&entry.path(), &mountpoint)
    }

    // We assume `entry` exists and either points to a path that exists *or*
    // points to a `/nix` path (which we'll attempt to resolve against `self.nixdir`).
    fn mirror_symlink<'p>(&self, entry: impl Into<DirEntryOrExplicitMount<'p>>) {
        let entry = entry.into();
        let link_path = self.rootdir.join(entry.file_name());
        if link_path.exists() {
            return;
        }
        let path = entry.path();

        // stops resolving the symlink at the first non-nix path
        let target = self
            .resolve_nix_path(path.clone(), true)
            .unwrap_or_else(|err| panic!("failed to resolve symlink {}: {}", &path.display(), err));

        log::info!(
            "MIRROR SYMLINK {} -> {}",
            target.display(),
            link_path.display()
        );

        symlink(&target, &link_path).unwrap_or_else(|err| {
            panic!(
                "failed to create symlink {} -> {} ({err:?})",
                &link_path.display(),
                &target.display()
            )
        });
    }

    fn bind_mount_entry<'p>(&self, entry: impl Into<DirEntryOrExplicitMount<'p>>) {
        use DirEntryOrExplicitMount::*;
        let mut entry = entry.into();

        // resolve any `/nix`s now so we can actually stat the file
        //
        // as with `mirror_symlink`, stop once we hit a non-nix path
        let adj_path;
        let dst_file_name;
        if entry.path().starts_with("/nix") {
            adj_path = self.resolve_nix_path(entry.path(), true).unwrap();
            entry = match entry {
                DirEntry(d) => {
                    dst_file_name = d.file_name();
                    ExplicitMount {
                        src: &adj_path,
                        dst_file_name: &dst_file_name,
                    }
                }
                ExplicitMount { dst_file_name, .. } => ExplicitMount {
                    src: &adj_path,
                    dst_file_name,
                },
            };
        }

        let path = entry.path();
        let stat = match entry.metadata() {
            Ok(m) => m,
            // TOCTOU: entry was listed by read_dir but has since disappeared
            // (common in /tmp, /run). Skip rather than abort the whole chroot.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                log::debug!(
                    "{} disappeared during mirror pass, skipping",
                    path.display()
                );
                return;
            }
            Err(e) => panic!("cannot get stat of {}: {}", path.display(), e),
        };

        if stat.is_dir() {
            self.bind_mount_directory(entry);
        } else if stat.is_file() || path == Path::new("/dev/null") {
            self.bind_mount_file(entry);
        } else if stat.file_type().is_symlink() {
            self.mirror_symlink(entry);
        } else {
            // Sockets, FIFOs, device nodes, etc. We can hit these when an
            // explicit mount causes the / mirror pass to recurse into a dir
            // like /tmp that contains them. Silently skipping matches
            // pre-PR behaviour and is harmless — the path just won't exist
            // in the chroot.
            log::debug!(
                "skipping special file {} (type {:?})",
                path.display(),
                stat.file_type()
            );
        }
    }

    fn run_chroot(
        &self,
        cmd: &str,
        args: &[String],
        path_config: Option<PathConfig>,
        map_root: bool,
    ) {
        let cwd = env::current_dir().expect("cannot get current working directory");

        let uid = unistd::getuid();
        let gid = unistd::getgid();

        unshare(CloneFlags::CLONE_NEWNS | CloneFlags::CLONE_NEWUSER).expect("unshare failed");

        // prepare pivot_root call:
        // rootdir must be a mount point
        mount(
            Some(self.rootdir),
            self.rootdir,
            Some("none"),
            MsFlags::MS_BIND | MsFlags::MS_REC,
            NONE,
        )
        .expect("failed to bind mount rootdir to itself");

        mount(
            Some(self.rootdir),
            self.rootdir,
            Some("none"),
            MsFlags::MS_PRIVATE | MsFlags::MS_REC,
            NONE,
        )
        .expect("failed to remount rootdir as private");

        // create /run/opengl-driver/lib in chroot, to behave like NixOS
        // (needed for nix pkgs with OpenGL or CUDA support to work)
        let ogldir = self.nixdir.join("var/nix/opengl-driver/lib");
        if ogldir.is_dir() {
            let ogl_mount = self.rootdir.join("run/opengl-driver/lib");
            fs::create_dir_all(&ogl_mount)
                .unwrap_or_else(|err| panic!("failed to create {}: {}", &ogl_mount.display(), err));
            bind_mount(&ogldir, &ogl_mount);
        }

        // TODO: test mounting in something to `/`; should work
        // TODO: test `cargo` or something else where the symlink's name is actually important (both as an explicit bind mount and an incidental one to make sure the logic is right)

        // mount in explicit mounts (profile relative, absolute, and placeholders to "reserve" the excludes):
        if let Some(ref c) = path_config {
            let user = unistd::User::from_uid(uid).unwrap().unwrap();
            let profile_dir = self
                .nixdir
                .join("var/nix/profiles/per-user")
                .join(&user.name)
                .join("profile");
            let profile_dir = self.resolve_nix_path(profile_dir, false);

            // Excludes go first so their /dev/null placeholders reserve the
            // destination paths before profile/absolute mounts (or the / mirror
            // pass below) can claim them.
            let explicit_mounts = c.excludes.paths
                .iter()
                .map(|ex| (PathBuf::from("/dev/null"), ex))
                .chain(
                    c.profile
                        .iter()
                        .filter(|(s, d)| if profile_dir.is_ok() {
                            true
                        } else {
                            log::warn!("couldn't find a profile for user `{}`; skipping profile mount `{}` -> `{}`", &user.name, s.display(), d.display());
                            false
                        })
                        .map(|(prof_p, chroot_p)| {
                            // to allow for both "absolute" and relative paths in the profile relative mounts
                            let prof_p = prof_p.strip_prefix("/").unwrap_or(prof_p);
                            (profile_dir.as_ref().unwrap().join(prof_p), chroot_p)
                        })
                )
                .chain(
                    c.absolute
                        .iter()
                        .inspect(|(src, _)| {
                            if !src.is_absolute() {
                                panic!("Explicit mount sources (excluding profile mounts) must be absolute paths! `{}` is not absolute.", src.display())
                            }
                        })
                        .map(|(src, dest)| (src.clone(), dest))
                )
                .inspect(|(_, dest)| {
                    if !dest.is_absolute() {
                        panic!("All explicit mount destinations must be absolute paths! `{}` is not absolute.", dest.display())
                    }
                });

            for (src, dest) in explicit_mounts {
                if let Ok(src) = self.resolve_nix_path(src.clone(), true) {
                    log::info!("EXPLICIT {} -> {}", src.display(), dest.display());

                    let adjusted_dest = dest
                        .strip_prefix("/") // we have guarantees that `dest` is absolute
                        .unwrap()
                        .parent()
                        .map(ToOwned::to_owned)
                        .unwrap_or_default();
                    let parent = self.rootdir.join(adjusted_dest);

                    fs::create_dir_all(&parent).unwrap();

                    let parent = self.with_rootdir(&parent);
                    parent.bind_mount_entry(
                        DirEntryOrExplicitMount::explicit_mount_with_dest_file_name(&src, dest),
                    );
                } else {
                    log::warn!(
                        "explicit mount source `{}` doesn't seem to exist!",
                        src.display()
                    );
                }
            }
        }

        // bind the rest of / stuff into rootdir
        let nix_root = PathBuf::from("/");
        let dir = fs::read_dir(&nix_root).expect("failed to list / directory");
        for entry in dir {
            let entry = entry.expect("error while listing from / directory");
            // do not bind mount an existing nix installation
            if entry.file_name() == "nix" {
                continue;
            }
            self.bind_mount_entry(&entry);
        }

        // remove the placeholders we used for the excludes
        if let Some(c) = path_config {
            for p in c.excludes.paths.iter() {
                let mount = self.rootdir.join(p.strip_prefix("/").unwrap());
                log::info!("UNBIND {}", mount.display());
                umount(&mount).unwrap();
            }
        }

        // mount the store
        let nix_mount = self.rootdir.join("nix");
        fs::create_dir(&nix_mount)
            .unwrap_or_else(|err| panic!("failed to create {}: {}", &nix_mount.display(), err));
        mount(
            Some(self.nixdir),
            &nix_mount,
            Some("none"),
            MsFlags::MS_BIND | MsFlags::MS_REC,
            NONE,
        )
        .unwrap_or_else(|err| {
            panic!(
                "failed to bind mount {} to /nix: {}",
                self.nixdir.display(),
                err
            )
        });

        // chroot
        unistd::pivot_root(self.rootdir, &nix_mount).unwrap_or_else(|err| {
            panic!(
                "pivot_root({}, {}): {}",
                self.rootdir.display(),
                &nix_mount.display(),
                err
            )
        });

        // mount the store and hide the old root we fetch nixdir under the old root
        let nix_store = nix_root.join(self.nixdir);
        mount(
            Some(&nix_store),
            "/nix",
            Some("none"),
            MsFlags::MS_BIND | MsFlags::MS_REC,
            NONE,
        )
        .unwrap_or_else(|_| panic!("failed to bind mount {} to /nix", nix_store.display()));

        env::set_current_dir("/").expect("cannot change directory to /");

        // fixes issue #1 where writing to /proc/self/gid_map fails
        // see user_namespaces(7) for more documentation
        if let Ok(mut file) = fs::File::create("/proc/self/setgroups") {
            let _ = file.write_all(b"deny");
        }

        // Normal operation maps our uid to itself so files created inside the
        // chroot are owned by the real user on the host. --install instead
        // maps us to root so nix-installer's EUID==0 checks pass; everything
        // it writes still lands as our real uid on the host filesystem.
        let inner_uid = if map_root { 0 } else { uid.as_raw() };
        let inner_gid = if map_root { 0 } else { gid.as_raw() };

        let mut uid_map =
            fs::File::create("/proc/self/uid_map").expect("failed to open /proc/self/uid_map");
        uid_map
            .write_all(format!("{inner_uid} {uid} 1").as_bytes())
            .expect("failed to write new uid mapping to /proc/self/uid_map");

        let mut gid_map =
            fs::File::create("/proc/self/gid_map").expect("failed to open /proc/self/gid_map");
        gid_map
            .write_all(format!("{inner_gid} {gid} 1").as_bytes())
            .expect("failed to write new gid mapping to /proc/self/gid_map");

        // restore cwd
        env::set_current_dir(&cwd)
            .unwrap_or_else(|_| panic!("cannot restore working directory {}", cwd.display()));

        let err = process::Command::new(cmd)
            .args(args)
            .env("NIX_CONF_DIR", "/nix/etc/nix")
            .exec();

        eprintln!("failed to execute {}: {}", &cmd, err);
        process::exit(1);
    }
}

fn wait_for_child(rootdir: &Path, child_pid: unistd::Pid) -> i32 {
    let mut exit_status = 1;
    loop {
        match waitpid(child_pid, Some(WaitPidFlag::WUNTRACED)) {
            Ok(WaitStatus::Signaled(child, Signal::SIGSTOP, _)) => {
                let _ = kill(unistd::getpid(), Signal::SIGSTOP);
                let _ = kill(child, Signal::SIGCONT);
            }
            Ok(WaitStatus::Signaled(_, signal, _)) => {
                kill(unistd::getpid(), signal).unwrap_or_else(|err| {
                    panic!("failed to send {signal} signal to our self: {err}")
                });
            }
            Ok(WaitStatus::Exited(_, status)) => {
                exit_status = status;
                break;
            }
            Ok(what) => {
                eprintln!("unexpected wait event happend: {what:?}");
                break;
            }
            Err(e) => {
                eprintln!("waitpid failed: {e}");
                break;
            }
        };
    }

    fs::remove_dir_all(rootdir)
        .unwrap_or_else(|err| panic!("cannot remove tempdir {}: {}", rootdir.display(), err));

    exit_status
}

fn main() {
    let mut builder = env_logger::Builder::new();
    builder
        .filter_level(log::LevelFilter::Warn)
        .parse_default_env()
        .init();

    let args: Vec<String> = env::args().collect();

    let (nixpath_arg, cmd, cmd_args, map_root, installing): (
        PathBuf,
        String,
        Vec<String>,
        bool,
        bool,
    ) = if args.get(1).map(String::as_str) == Some("--install") {
        let nixpath = args
            .get(2)
            .map(PathBuf::from)
            .unwrap_or_else(install::default_nixpath);

        if nixpath.join("store").exists() {
            eprintln!(
                "{}: store already exists, refusing to reinstall.",
                nixpath.display()
            );
            eprintln!(
                "Enter it with: nix-user-chroot {} bash -l",
                nixpath.display()
            );
            process::exit(1);
        }

        fs::create_dir_all(&nixpath).unwrap_or_else(|e| {
            eprintln!("failed to create {}: {e}", nixpath.display());
            process::exit(1);
        });

        let installer = install::fetch_installer().unwrap_or_else(|e| {
            eprintln!("failed to fetch nix-installer: {e}");
            process::exit(1);
        });
        let (cmd, cmd_args) = install::installer_command(&installer);
        (nixpath, cmd, cmd_args, true, true)
    } else {
        if args.len() < 3 {
            eprintln!("Usage: {} <nixpath> <command>", args[0]);
            eprintln!("       {} --install [nixpath]", args[0]);
            process::exit(1);
        }
        (
            PathBuf::from(&args[1]),
            args[2].clone(),
            args[3..].to_vec(),
            false,
            false,
        )
    };

    let rootdir = mkdtemp::mkdtemp("nix-chroot.XXXXXX")
        .unwrap_or_else(|err| panic!("failed to create temporary directory: {err}"));

    let nixdir = fs::canonicalize(&nixpath_arg).unwrap_or_else(|err| {
        panic!(
            "failed to resolve nix directory {}: {}",
            nixpath_arg.display(),
            err
        )
    });

    let path_config_file_path = nixdir.join("etc/nix-user-chroot/path-config.toml");
    let path_config: Option<PathConfig> = if path_config_file_path.exists() {
        let contents = fs::read_to_string(&path_config_file_path).unwrap_or_else(|e| {
            eprintln!(
                "failed to read config file {}: {}",
                path_config_file_path.display(),
                e
            );
            process::exit(1);
        });
        match toml::from_str(&contents) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                eprintln!(
                    "failed to parse config file {}: {}",
                    path_config_file_path.display(),
                    e
                );
                process::exit(1);
            }
        }
    } else {
        None
    };

    if let Some(ref c) = path_config {
        for p in &c.excludes.paths {
            if !p.is_absolute() {
                eprintln!(
                    "exclude path `{}` must be absolute (in {})",
                    p.display(),
                    path_config_file_path.display()
                );
                process::exit(1);
            }
        }
    }

    match unsafe { fork() } {
        Ok(ForkResult::Parent { child, .. }) => {
            let status = wait_for_child(&rootdir, child);
            if installing && status == 0 {
                install::print_next_steps(&nixpath_arg);
            }
            process::exit(status);
        }
        Ok(ForkResult::Child) => {
            RunChroot::new(&rootdir, &nixdir).run_chroot(&cmd, &cmd_args, path_config, map_root)
        }
        Err(e) => {
            eprintln!("fork failed: {e}");
            process::exit(1);
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify resolve_nix_path rewrites /nix symlinks found in intermediate
    /// path components, not just the final one.
    #[test]
    fn resolve_intermediate_nix_symlink() {
        let tmp = mkdtemp::mkdtemp("nix-user-chroot-test.XXXXXX").unwrap();

        // Simulate a nix store: tmp/store/foo/bar exists
        let nixdir = tmp.join("nixroot");
        let real_target = nixdir.join("store/foo/bar");
        fs::create_dir_all(real_target.parent().unwrap()).unwrap();
        fs::write(&real_target, b"hello").unwrap();

        // A symlink that points into /nix (which doesn't exist on the host
        // during tests): tmp/link -> /nix/store/foo
        let link = tmp.join("link");
        symlink("/nix/store/foo", &link).unwrap();

        // rootdir is unused by resolve_nix_path, supply a dummy
        let rc = RunChroot::new(&tmp, &nixdir);

        // Input path goes through the symlink: tmp/link/bar
        // Expected: /nix/store/foo gets rewritten to nixdir/store/foo,
        // yielding nixdir/store/foo/bar.
        let input = link.join("bar");
        let resolved = rc.resolve_nix_path(input, false).unwrap();
        assert_eq!(resolved, real_target);

        // A deeper chain: link2 -> /nix/store, then link2/foo/bar
        let link2 = tmp.join("link2");
        symlink("/nix/store", &link2).unwrap();
        let resolved = rc.resolve_nix_path(link2.join("foo/bar"), false).unwrap();
        assert_eq!(resolved, real_target);

        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn resolve_missing_path_no_nix_symlink() {
        let tmp = mkdtemp::mkdtemp("nix-user-chroot-test.XXXXXX").unwrap();
        let nixdir = tmp.join("nixroot");
        fs::create_dir_all(&nixdir).unwrap();

        let rc = RunChroot::new(&tmp, &nixdir);
        let err = rc
            .resolve_nix_path(tmp.join("does/not/exist"), false)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);

        fs::remove_dir_all(&tmp).unwrap();
    }
}
