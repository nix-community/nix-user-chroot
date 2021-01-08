use nix::sched::{unshare, CloneFlags};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd;
use nix::unistd::{fork, ForkResult};
use std::env;
use std::fs;
use std::io::prelude::*;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::string::String;
use tempfile::TempDir;

use rooter::Rooter;

fn run_chroot(nixdir: &Path, rootdir: &Path, cmd: &str, args: &[String]) {
    let uid = unistd::getuid();
    let gid = unistd::getgid();

    unshare(CloneFlags::CLONE_NEWNS | CloneFlags::CLONE_NEWUSER).expect("unshare failed");

    // prepare pivot_root call:
    // rootdir must be a mount point

    let mut rooter = Rooter::new(rootdir.to_owned());

    // bind mount all / stuff into rootdir
    // the orginal content of / now available under /nix
    let nix_root = PathBuf::from("/");
    let dir = fs::read_dir(&nix_root).expect("failed to list / directory");
    for entry in dir {
        let entry = entry.expect("failed to read directory entry");
        let path = Path::new("/").join(entry.file_name());
        rooter.bind_self(path).expect("failed to bind host directory");
    }

    rooter
        .bind_dir(nixdir, "/nix").expect("failed to bind /nix")
        .preserve_cwd(true);

    rooter.chroot().expect("failed to change root directory");

    // fixes issue #1 where writing to /proc/self/gid_map fails
    // see user_namespaces(7) for more documentation
    if let Ok(mut file) = fs::File::create("/proc/self/setgroups") {
        let _ = file.write_all(b"deny");
    }

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

    let err = process::Command::new(cmd)
        .args(args)
        .env("NIX_CONF_DIR", "/nix/etc/nix")
        .exec();

    eprintln!("failed to execute {}: {}", &cmd, err);
    process::exit(1);
}

fn wait_for_child(child_pid: unistd::Pid, tempdir: TempDir, rootdir: &Path) {
    loop {
        match waitpid(child_pid, Some(WaitPidFlag::WUNTRACED)) {
            Ok(WaitStatus::Signaled(child, Signal::SIGSTOP, _)) => {
                let _ = kill(unistd::getpid(), Signal::SIGSTOP);
                let _ = kill(child, Signal::SIGCONT);
            }
            Ok(WaitStatus::Signaled(_, signal, _)) => {
                kill(unistd::getpid(), signal)
                    .unwrap_or_else(|_| panic!("failed to send {} signal to our self", signal));
            }
            Ok(WaitStatus::Exited(_, status)) => {
                tempdir.close().unwrap_or_else(|_| {
                    panic!(
                        "failed to remove temporary directory: {}",
                        rootdir.display()
                    )
                });
                process::exit(status);
            }
            Ok(what) => {
                tempdir.close().unwrap_or_else(|_| {
                    panic!(
                        "failed to remove temporary directory: {}",
                        rootdir.display()
                    )
                });
                eprintln!("unexpected wait event happend: {:?}", what);
                process::exit(1);
            }
            Err(e) => {
                tempdir.close().unwrap_or_else(|_| {
                    panic!(
                        "failed to remove temporary directory: {}",
                        rootdir.display()
                    )
                });
                eprintln!("waitpid failed: {}", e);
                process::exit(1);
            }
        };
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <nixpath> <command>\n", args[0]);
        process::exit(1);
    }
    let tempdir =
        TempDir::new().expect("failed to create temporary directory for mount point");
    let rootdir = PathBuf::from(tempdir.path());

    let nixdir = fs::canonicalize(&args[1])
        .unwrap_or_else(|_| panic!("failed to resolve nix directory {}", &args[1]));

    match fork() {
        Ok(ForkResult::Parent { child, .. }) => wait_for_child(child, tempdir, &rootdir),
        Ok(ForkResult::Child) => run_chroot(&nixdir, &rootdir, &args[2], &args[3..]),
        Err(e) => {
            eprintln!("fork failed: {}", e);
        }
    };
}
