use nix::errno::Errno;
use std::env;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr;

mod ffi {
    extern "C" {
        pub fn mkdtemp(template: *mut libc::c_char) -> *mut libc::c_char;
    }
}

pub fn mkdtemp(template: &str) -> nix::Result<PathBuf> {
    let mut tmpdir = env::temp_dir();
    tmpdir.push(template);
    let mut buf = tmpdir.into_os_string().into_vec();
    buf.push(b'\0'); // make a c string

    let res = unsafe { ffi::mkdtemp(buf.as_mut_ptr() as *mut libc::c_char) };
    if res == ptr::null_mut() {
        Err(nix::Error::Sys(Errno::last()))
    } else {
        buf.pop(); // strip null byte
        Ok(PathBuf::from(OsString::from_vec(buf)))
    }
}
