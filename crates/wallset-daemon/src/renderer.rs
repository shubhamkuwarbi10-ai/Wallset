//! Owning one `wallset-render` child process.
//!
//! * Every child goes into a Job Object with `KILL_ON_JOB_CLOSE`, so if the
//!   daemon dies, Windows kills the renderers. No orphans holding 300 MB.
//! * Exit is detected with `RegisterWaitForSingleObject`: a thread-pool wait
//!   in the kernel that posts a window message when the process ends. No
//!   polling, and no thread of our own parked per child.

use std::ffi::c_void;
use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, WPARAM};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, INFINITE, RegisterWaitForSingleObject, UnregisterWaitEx, WT_EXECUTEONLYONCE,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::PCWSTR;

/// Window that receives `WM_APP_CHILD_EXIT`. Set once at startup.
pub static NOTIFY_HWND: AtomicIsize = AtomicIsize::new(0);
pub const WM_APP_CHILD_EXIT: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

/// Kills every renderer when the daemon's last handle to it closes,
/// including when the daemon crashes.
#[derive(Debug)]
pub struct Job(HANDLE);

impl Job {
    pub fn new() -> windows::core::Result<Self> {
        // SAFETY: anonymous job object.
        let h = unsafe { CreateJobObjectW(None, PCWSTR::null()) }?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: struct and size match the information class.
        unsafe {
            SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }?;
        Ok(Self(h))
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: our handle; closing it kills any remaining members.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Everything the renderer needs to be told on its command line.
#[derive(Debug, Clone)]
pub struct Launch<'a> {
    pub exe: &'a Path,
    pub wid: isize,
    pub file: &'a Path,
    pub start_secs: Option<f64>,
    /// From `RenderSettings::renderer_args`.
    pub args: Vec<String>,
    pub stderr_log: Option<&'a Path>,
}

#[derive(Debug)]
pub struct Renderer {
    child: Child,
    wait: HANDLE,
    pub paused: bool,
}

unsafe extern "system" fn on_exit(ctx: *mut c_void, _timed_out: bool) {
    let hwnd = NOTIFY_HWND.load(Ordering::Acquire);
    if hwnd != 0 {
        // SAFETY: posting to our own window; ctx carries the pid.
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_CHILD_EXIT, WPARAM(ctx as usize), LPARAM(0));
        }
    }
}

impl Renderer {
    pub fn spawn(l: &Launch<'_>, job: &Job) -> std::io::Result<Self> {
        let mut cmd = Command::new(l.exe);
        cmd.args(["--wid", &l.wid.to_string(), "--file"])
            .arg(l.file)
            .arg("--show-on-ready")
            .args(&l.args);
        if let Some(s) = l.start_secs {
            cmd.args(["--start", &format!("{s:.3}")]);
        }
        let stderr = match l.stderr_log {
            Some(p) => std::fs::OpenOptions::new().create(true).append(true).open(p).map(Stdio::from).unwrap_or_else(|_| Stdio::null()),
            None => Stdio::null(),
        };
        let child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(stderr)
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()?;

        let proc = HANDLE(child.as_raw_handle());
        // SAFETY: both handles are valid; a failure only weakens orphan cleanup.
        let _ = unsafe { AssignProcessToJobObject(job.0, proc) };

        let mut wait = HANDLE::default();
        // SAFETY: `proc` stays valid while `child` lives, and Drop unregisters
        // the wait (blocking until any in-flight callback completes) before
        // `child` is dropped.
        unsafe {
            RegisterWaitForSingleObject(
                &mut wait,
                proc,
                Some(on_exit),
                Some(child.id() as usize as *const c_void),
                INFINITE,
                WT_EXECUTEONLYONCE,
            )
        }
        .map_err(std::io::Error::other)?;

        Ok(Self { child, wait, paused: false })
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Send one protocol line (see wallset-render's module docs).
    pub fn send(&mut self, line: &str) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, "{line}").and_then(|()| stdin.flush());
        }
    }

    pub fn pause(&mut self) {
        if !self.paused {
            self.send("pause");
            self.paused = true;
        }
    }

    pub fn resume(&mut self) {
        if self.paused {
            self.send("resume");
            self.paused = false;
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: INVALID_HANDLE_VALUE makes this wait for any running
        // callback, so it cannot fire after `child` (and its handle) is gone.
        unsafe {
            let _ = UnregisterWaitEx(self.wait, Some(INVALID_HANDLE_VALUE));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
