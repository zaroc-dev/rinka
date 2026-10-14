//! Named pipe transport.
//!
//! One blocking byte-mode pipe instance per client. The listener always keeps
//! a spare instance open, so a new client finds one waiting.

use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_PIPE_CONNECTED, FALSE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const BUFFER: u32 = 64 * 1024;

/// Default pipe: `\\.\pipe\rinka-host-<user>`. Named pipes share one
/// machine-wide namespace, so the user name keeps users' hosts apart.
pub fn default_endpoint() -> anyhow::Result<PathBuf> {
    let user = std::env::var("USERNAME").context("USERNAME is not set")?;
    Ok(PathBuf::from(format!(r"\\.\pipe\rinka-host-{user}")))
}

/// A named pipe whose next instance waits for a client.
pub struct PipeListener {
    name: Vec<u16>,
    security: Security,
    next: OwnedHandle,
}

/// Creates the first pipe instance, refusing to start when another host
/// already owns the name.
pub fn bind(path: &Path) -> anyhow::Result<PipeListener> {
    let text = path.to_string_lossy();
    if !text.starts_with(r"\\.\pipe\") {
        bail!(r"{text} is not a named pipe path (\\.\pipe\...)");
    }
    let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let security = Security::current_user().context("building the pipe's access rules")?;
    let next = create(&name, &security, true).map_err(|e| {
        if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
            anyhow::anyhow!("another host is already listening on {text}")
        } else {
            anyhow::anyhow!("creating {text}: {e}")
        }
    })?;
    Ok(PipeListener {
        name,
        security,
        next,
    })
}

impl super::Listener for PipeListener {
    type Reader = std::fs::File;
    type Writer = Flushing;

    fn accept(&mut self) -> std::io::Result<(std::fs::File, Flushing)> {
        if let Err(e) = connect(&self.next) {
            // Early disconnects leave ERROR_NO_DATA until reset.
            // SAFETY: a valid named pipe handle owned by the listener.
            unsafe { DisconnectNamedPipe(self.next.as_raw_handle()) };
            return Err(e);
        }
        // Open the next instance before serving this one, so a new client
        // always finds a listening instance.
        let fresh = create(&self.name, &self.security, false)?;
        let connected = std::fs::File::from(std::mem::replace(&mut self.next, fresh));
        let writer = connected.try_clone()?;
        Ok((connected, Flushing(writer)))
    }
}

/// Creates a blocking byte-mode pipe instance that only the current user and
/// SYSTEM can open. Remote clients are refused.
fn create(name: &[u16], security: &Security, first: bool) -> std::io::Result<OwnedHandle> {
    let flags = PIPE_ACCESS_DUPLEX
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0,
        bInheritHandle: FALSE,
    };
    // SAFETY: `name` is NUL-terminated and `attributes` outlives the call;
    // the returned handle is checked and owned by the result.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            flags,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER,
            BUFFER,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a valid handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// Waits for a client to connect to `pipe`.
fn connect(pipe: &OwnedHandle) -> std::io::Result<()> {
    // SAFETY: a valid pipe handle, synchronous (no OVERLAPPED).
    if unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) } != 0 {
        return Ok(());
    }
    // A client that connected between creation and this call.
    // SAFETY: reads the calling thread's last error.
    match unsafe { GetLastError() } {
        ERROR_PIPE_CONNECTED => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

/// Flushes each reply so a client reads it even if the host exits right
/// after writing it (the `shutdown` reply).
pub struct Flushing(std::fs::File);

impl Write for Flushing {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // SAFETY: a valid pipe handle owned by `self.0`.
        if unsafe { FlushFileBuffers(self.0.as_raw_handle()) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// A security descriptor granting access to the current user and SYSTEM
/// only. The default pipe descriptor also lets Everyone read.
struct Security(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is immutable after creation and only read by
// CreateNamedPipeW.
unsafe impl Send for Security {}

impl Security {
    fn current_user() -> std::io::Result<Self> {
        let sid = current_user_sid()?;
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)")
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: NUL-terminated SDDL; the descriptor is freed in `Drop`.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

/// The process user's SID in string form (`S-1-5-21-…`).
fn current_user_sid() -> std::io::Result<String> {
    // SAFETY: token query calls with checked results; the token handle is
    // closed and the SID string freed before returning.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        // u64 elements keep the buffer aligned for TOKEN_USER.
        let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), len, &mut len);
        let error = std::io::Error::last_os_error();
        CloseHandle(token);
        if ok == 0 {
            return Err(error);
        }
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut text = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        Ok(sid)
    }
}
