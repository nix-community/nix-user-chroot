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
use serde_derive::Deserialize;

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
        dst_file_name: Option<&'a OsStr>,
    },
}

impl<'a> From<&'a DirEntry> for DirEntryOrExplicitMount<'a> {
    fn from(de: &'a DirEntry) -> DirEntryOrExplicitMount {
        DirEntryOrExplicitMount::DirEntry(de)
    }
}

impl<'a> DirEntryOrExplicitMount<'a> {
    fn explicit_mount_with_dest_file_name(
        mount: &'a Path,
        dst_file: &'a (impl AsRef<Path> + 'a),
    ) -> Self {
        DirEntryOrExplicitMount::ExplicitMount {
            src: mount,
            dst_file_name: dst_file.as_ref().file_name(),
        }
    }
}

impl DirEntryOrExplicitMount<'_> {
    fn file_name(&self) -> Option<OsString> {
        use DirEntryOrExplicitMount::*;

        match self {
            DirEntry(d) => Some(d.file_name()),
            ExplicitMount { dst_file_name, .. } => dst_file_name.map(|p| p.to_owned()),
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PathConfig<'a> {
    excludes: ExcludePaths<'a>,
    #[serde(borrow)]
    profile: HashMap<&'a Path, &'a Path>,
    #[serde(borrow)]
    absolute: HashMap<&'a Path, &'a Path>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ExcludePaths<'a> {
    #[serde(borrow)]
    paths: HashSet<&'a Path>,
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

            self.resolve_nix_path(p, stop_at_first_non_nix_path)
        } else if p.exists() {
            Ok(p)
        } else {
            // peel off components of the path, seeing if at some point we
            // hit a symlink containing `/nix` which would explain why we
            // couldn't stat the file
            let mut i = 0;
            let mut path = p.clone();

            // NOTE: this is the bad N^2 way of doing this; we should actually
            // resolve the path from the root onwards
            while path.pop() {
                i += 1;

                if path.is_symlink()
                    && path
                        .read_link()
                        .map(|p| p.starts_with("/nix"))
                        .unwrap_or(false)
                {
                    // if we did find a parent that's a symlink, resolve it:
                    let actual_parent = self.resolve_nix_path(path, stop_at_first_non_nix_path)?;

                    // append the components we stripped off to the resolved parent:
                    let parts = p.iter().collect::<Vec<_>>();
                    let stripped = &parts[parts.len() - i..];
                    let path = actual_parent.join(stripped.iter().collect::<PathBuf>());

                    // and try again:
                    return self.resolve_nix_path(path, stop_at_first_non_nix_path);
                }
            }

            Err(io::ErrorKind::NotFound.into())
        }
    }

    // We assume `entry` exists and is actually a directory (not a file or symlink),
    fn bind_mount_directory<'p>(&self, entry: impl Into<DirEntryOrExplicitMount<'p>>) {
        let entry = entry.into();
        let mountpoint = self.rootdir.join(entry.file_name().unwrap_or_default());

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
                let dir = fs::read_dir(entry.path()).unwrap_or_else(|err| {
                    panic!("failed to list dir {}: {}", entry.path().display(), err)
                });

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
        let mountpoint = self.rootdir.join(entry.file_name().unwrap_or_default());
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
        let link_path = self.rootdir.join(entry.file_name().unwrap_or_default());
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
                        src: &*adj_path,
                        dst_file_name: Some(&dst_file_name),
                    }
                }
                ExplicitMount { dst_file_name, .. } => ExplicitMount {
                    src: &*adj_path,
                    dst_file_name,
                },
            };
        }

        let path = entry.path();
        let stat = entry
            .metadata()
            .unwrap_or_else(|err| panic!("cannot get stat of {}: {}", path.display(), err));

        if stat.is_dir() {
            self.bind_mount_directory(entry);
        } else if stat.is_file() || path == Path::new("/dev/null") {
            self.bind_mount_file(entry);
        } else if stat.file_type().is_symlink() {
            self.mirror_symlink(entry);
        } else {
            panic!("don't know what to do with: {}", path.display())
        }
    }

    fn run_chroot(&self, cmd: &str, args: &[String], path_config: Option<PathConfig<'_>>) {
        let cwd = env::current_dir().expect("cannot get current working directory");

        let uid = unistd::getuid();
        let gid = unistd::getgid();

        unshare(CloneFlags::CLONE_NEWNS | CloneFlags::CLONE_NEWUSER).expect("unshare failed");

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

            let explicit_mounts = c.profile
                .iter()
                .map(|(s, d)| (*s, *d))
                .filter(|(s, d)| if profile_dir.is_ok() {
                    true
                } else {
                    eprintln!("Warning: couldn't find a profile for user `{}`; skipping profile mount `{}` -> `{}`", &user.name, s.display(), d.display());
                    false
                })
                .map(|(mut prof_p, chroot_p)| {
                    // to allow for both "absolute" and relative paths in the profile relative mounts
                    if prof_p.is_absolute() {
                        prof_p = prof_p.strip_prefix("/").unwrap()
                    }

                    (prof_p, chroot_p)
                })
                .map(|(prof_p, chroot_p)| (profile_dir.as_ref().unwrap().join(prof_p), chroot_p))
                .chain(
                    // TODO: this should actually probably happen first.
                    c.excludes.paths
                        .iter()
                        .map(|&ex| (PathBuf::from("/dev/null"), ex))
                )
                .chain(
                    c.absolute
                        .iter()
                        .map(|(s, d)| (*s, *d))
                        .inspect(|(src, _)| {
                            if !src.is_absolute() {
                                panic!("Explicit mount sources (excluding profile mounts) must be absolute paths! `{}` is not absolute.", src.display())
                            }
                        })
                        .map(|(src, dest)| {
                            (src.to_owned(), dest)
                        })
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
                        DirEntryOrExplicitMount::explicit_mount_with_dest_file_name(&*src, &dest),
                    );
                } else {
                    eprintln!(
                        "warning: explicit mount source `{}` doesn't seem to exist!",
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
            for &p in c.excludes.paths.iter() {
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
        unistd::chroot(self.rootdir)
            .unwrap_or_else(|err| panic!("chroot({}): {}", self.rootdir.display(), err));

        env::set_current_dir("/").expect("cannot change directory to /");

        // fixes issue #1 where writing to /proc/self/gid_map fails
        // see user_namespaces(7) for more documentation
        if let Ok(mut file) = fs::File::create("/proc/self/setgroups") {
            let _ = file.write_all(b"deny");
        }

        // println!("cap: {}", std::fs::read_to_string(format!("/proc/self/status")).unwrap());

        let mut uid_map =
            fs::File::create("/proc/self/uid_map").expect("failed to open /proc/self/uid_map");
        uid_map
            .write_all(format!("{} {} 1", uid, uid).as_bytes())
            .expect("failed to write new uid mapping to /proc/self/uid_map");

        let mut gid_map =
            fs::File::create("/proc/self/gid_map").expect("failed to open /proc/self/gid_map");
        gid_map
            .write_all(format!("{} {} 1", gid, gid).as_bytes())
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

fn wait_for_child(rootdir: &Path, child_pid: unistd::Pid) -> ! {
    let mut exit_status = 1;
    loop {
        match waitpid(child_pid, Some(WaitPidFlag::WUNTRACED)) {
            Ok(WaitStatus::Signaled(child, Signal::SIGSTOP, _)) => {
                let _ = kill(unistd::getpid(), Signal::SIGSTOP);
                let _ = kill(child, Signal::SIGCONT);
            }
            Ok(WaitStatus::Signaled(_, signal, _)) => {
                kill(unistd::getpid(), signal).unwrap_or_else(|err| {
                    panic!("failed to send {} signal to our self: {}", signal, err)
                });
            }
            Ok(WaitStatus::Exited(_, status)) => {
                exit_status = status;
                break;
            }
            Ok(what) => {
                eprintln!("unexpected wait event happend: {:?}", what);
                break;
            }
            Err(e) => {
                eprintln!("waitpid failed: {}", e);
                break;
            }
        };
    }

    fs::remove_dir_all(rootdir)
        .unwrap_or_else(|err| panic!("cannot remove tempdir {}: {}", rootdir.display(), err));

    process::exit(exit_status);
}

fn main() {
    let mut builder = env_logger::Builder::new();
    builder
        .filter_level(log::LevelFilter::Warn)
        .parse_default_env()
        .init();

    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <nixpath> <command>\n", args[0]);
        process::exit(1);
    }

    let rootdir = mkdtemp::mkdtemp("nix-chroot.XXXXXX")
        .unwrap_or_else(|err| panic!("failed to create temporary directory: {}", err));

    let nixdir = fs::canonicalize(&args[1])
        .unwrap_or_else(|err| panic!("failed to resolve nix directory {}: {}", &args[1], err));

    let path_config_file_path = nixdir.join("etc/nix-user-chroot/path-config.toml");
    let config_file;
    let config_file = if path_config_file_path.exists() {
        config_file = fs::read_to_string(path_config_file_path).unwrap();
        Some(toml::from_str(&*config_file).unwrap())
    } else {
        None
    };

    match unsafe { fork() } {
        Ok(ForkResult::Parent { child, .. }) => wait_for_child(&rootdir, child),
        Ok(ForkResult::Child) => {
            RunChroot::new(&rootdir, &nixdir).run_chroot(&args[2], &args[3..], config_file)
        }
        Err(e) => {
            eprintln!("fork failed: {}", e);
        }
    };
}
