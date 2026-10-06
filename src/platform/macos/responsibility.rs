//! Makes a llama-cu inside llama-cu.app responsible for itself, so macOS
//! charges Accessibility and Screen Recording to the app instead of the
//! terminal or agent host that started it.

use std::env;
use std::ffi::{CString, c_char, c_int, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use crate::error::{Error, ErrorCode, Result};

/// Set in the environment of the re-executed process so it does not hand
/// off again.
const HANDED_OFF: &str = "LLAMA_CU_HANDED_OFF";

/// Apple's `posix_spawn` flag that replaces the calling process instead of
/// creating a new one.
const POSIX_SPAWN_SETEXEC: i16 = 0x0040;

/// `dlsym` handle that searches every loaded image.
const RTLD_DEFAULT: *mut c_void = ptr::without_provenance_mut(-2isize as usize);

/// Signature of `responsibility_spawnattrs_setdisclaim`, a private but
/// long-stable libSystem call. With `disclaim` set, the spawned process
/// becomes responsible for itself. Chromium uses it for its helper
/// processes. It is looked up at run time, so a macOS without it fails only
/// the hand-off instead of every launch.
type SetDisclaim = unsafe extern "C" fn(attr: *mut *mut c_void, disclaim: c_int) -> c_int;

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn posix_spawnattr_init(attr: *mut *mut c_void) -> c_int;
    fn posix_spawnattr_setflags(attr: *mut *mut c_void, flags: i16) -> c_int;
    fn posix_spawnattr_destroy(attr: *mut *mut c_void) -> c_int;
    fn posix_spawn(
        pid: *mut c_int,
        path: *const c_char,
        file_actions: *const c_void,
        attr: *const *mut c_void,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> c_int;
}

/// Re-executes the process in place, responsible for itself, when it runs
/// from inside an app bundle. Returns without doing anything otherwise, or
/// after the hand-off already happened. On success it does not return.
pub fn become_responsible() -> Result<()> {
    if env::var_os(HANDED_OFF).is_some() {
        // SAFETY: no other threads exist yet.
        unsafe { env::remove_var(HANDED_OFF) };
        return Ok(());
    }
    let Some(exe) = bundled_executable() else {
        return Ok(());
    };
    let path = c_string(exe.as_os_str().as_bytes())?;
    let args = env::args_os()
        .map(|a| c_string(a.as_bytes()))
        .collect::<Result<Vec<_>>>()?;
    let mut vars = env::vars_os()
        .map(|(k, v)| c_string(&[k.as_bytes(), b"=", v.as_bytes()].concat()))
        .collect::<Result<Vec<_>>>()?;
    vars.push(c_string(format!("{HANDED_OFF}=1").as_bytes())?);
    let argv = null_terminated(&args);
    let envp = null_terminated(&vars);
    let set_disclaim = set_disclaim()?;

    // SAFETY: every pointer refers to a live, NUL-terminated buffer, and
    // the attribute is initialized before use and destroyed after.
    let err = unsafe {
        let mut attr: *mut c_void = ptr::null_mut();
        let mut err = posix_spawnattr_init(&mut attr);
        if err == 0 {
            err = posix_spawnattr_setflags(&mut attr, POSIX_SPAWN_SETEXEC);
            if err == 0 {
                err = set_disclaim(&mut attr, 1);
            }
            if err == 0 {
                let mut pid = 0;
                err = posix_spawn(
                    &mut pid,
                    path.as_ptr(),
                    ptr::null(),
                    &attr,
                    argv.as_ptr(),
                    envp.as_ptr(),
                );
            }
            posix_spawnattr_destroy(&mut attr);
        }
        err
    };
    Err(Error::new(
        ErrorCode::Platform,
        format!(
            "re-running {} so the app owns its permissions failed: {}",
            exe.display(),
            io::Error::from_raw_os_error(err)
        ),
    ))
}

fn set_disclaim() -> Result<SetDisclaim> {
    // SAFETY: the name is NUL-terminated.
    let symbol = unsafe {
        dlsym(
            RTLD_DEFAULT,
            c"responsibility_spawnattrs_setdisclaim".as_ptr(),
        )
    };
    if symbol.is_null() {
        return Err(Error::new(
            ErrorCode::Unsupported,
            "this macOS cannot give llama-cu.app its own permissions; \
             run the llama-cu binary from outside the app",
        ));
    }
    // SAFETY: the symbol is the libSystem function with this signature.
    Ok(unsafe { std::mem::transmute::<*mut c_void, SetDisclaim>(symbol) })
}

/// Returns the app bundle this process runs from, if any.
pub fn bundle() -> Option<PathBuf> {
    bundled_executable().and_then(|exe| bundle_of(&exe).map(Path::to_path_buf))
}

/// Returns the real path of this executable when it is inside an app
/// bundle, following symlinks such as one on the `PATH`.
fn bundled_executable() -> Option<PathBuf> {
    let exe = env::current_exe().and_then(|p| p.canonicalize()).ok()?;
    bundle_of(&exe).is_some().then_some(exe)
}

/// Returns the bundle of a main executable at `<name>.app/Contents/MacOS/`.
fn bundle_of(exe: &Path) -> Option<&Path> {
    let macos = exe.parent()?;
    let bundle = macos.parent()?.parent()?;
    let is_bundle =
        macos.ends_with("Contents/MacOS") && bundle.extension().is_some_and(|ext| ext == "app");
    is_bundle.then_some(bundle)
}

fn c_string(bytes: &[u8]) -> Result<CString> {
    CString::new(bytes).map_err(|_| {
        Error::new(
            ErrorCode::InvalidArgument,
            "arguments and environment variables cannot contain NUL bytes",
        )
    })
}

fn null_terminated(strings: &[CString]) -> Vec<*const c_char> {
    strings
        .iter()
        .map(|s| s.as_ptr())
        .chain([ptr::null()])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_detection() {
        let cases = [
            (
                "bundled",
                "/Users/troy/Applications/llama-cu.app/Contents/MacOS/llama-cu",
                Some("/Users/troy/Applications/llama-cu.app"),
            ),
            ("cargo build", "/src/llama-cu/target/release/llama-cu", None),
            ("not an app", "/opt/greendale/Contents/MacOS/llama-cu", None),
            (
                "helper inside a bundle",
                "/A.app/Contents/Helpers/llama-cu",
                None,
            ),
        ];
        for (name, exe, want) in cases {
            assert_eq!(bundle_of(Path::new(exe)), want.map(Path::new), "{name}");
        }
    }
}
