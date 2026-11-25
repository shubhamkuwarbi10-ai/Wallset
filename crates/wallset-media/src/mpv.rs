//! Minimal, safe wrapper over the libmpv client API, loaded at runtime.
//!
//! Only the dozen entry points wallSet needs are bound. Loading dynamically
//! rather than linking means:
//!
//! * no import library is needed at build time;
//! * a machine without libmpv still runs wallSet, with video disabled and a
//!   clear message, rather than failing to start;
//! * the ABI version is checked before anything is called.

use std::ffi::{CStr, CString, c_char, c_double, c_int, c_ulong, c_void};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// libmpv's client API major version this binding was written against.
/// `libmpv-2.dll` / `libmpv.so.2` both report major 2.
const REQUIRED_API_MAJOR: c_ulong = 2;

#[derive(Debug, thiserror::Error)]
pub enum MpvError {
    #[error("libmpv could not be loaded (tried: {tried}). Video wallpapers need libmpv; still images work without it.")]
    NotFound { tried: String },
    #[error("libmpv is missing symbol `{0}`; the library is too old or not libmpv")]
    MissingSymbol(&'static str),
    #[error("libmpv client API {found_major}.{found_minor} is not supported (need {REQUIRED_API_MAJOR}.x)")]
    WrongVersion { found_major: c_ulong, found_minor: c_ulong },
    #[error("mpv_create returned null (out of memory?)")]
    CreateFailed,
    #[error("mpv error {code} during `{context}`: {message}")]
    Call { code: c_int, context: String, message: String },
    #[error("string contains an interior NUL byte")]
    Nul(#[from] std::ffi::NulError),
    #[error("timed out after {0:?} waiting for mpv")]
    Timeout(Duration),
    #[error("mpv could not open the file: {0}")]
    LoadFailed(String),
}

type MpvHandle = c_void;

#[repr(C)]
struct RawEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct RawEndFile {
    reason: c_int,
    error: c_int,
}

/// The function table. Kept separate from [`Mpv`] so one loaded library can
/// back several handles (the probe and a preview, say).
struct Api {
    client_api_version: unsafe extern "C" fn() -> c_ulong,
    create: unsafe extern "C" fn() -> *mut MpvHandle,
    initialize: unsafe extern "C" fn(*mut MpvHandle) -> c_int,
    terminate_destroy: unsafe extern "C" fn(*mut MpvHandle),
    set_option_string: unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int,
    set_property_string:
        unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int,
    get_property_string: unsafe extern "C" fn(*mut MpvHandle, *const c_char) -> *mut c_char,
    command: unsafe extern "C" fn(*mut MpvHandle, *mut *const c_char) -> c_int,
    wait_event: unsafe extern "C" fn(*mut MpvHandle, c_double) -> *mut RawEvent,
    free: unsafe extern "C" fn(*mut c_void),
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    // Keep the library mapped for as long as any function pointer exists.
    _lib: libloading::Library,
}

/// A loaded libmpv. Cheap to clone.
#[derive(Clone)]
pub struct Library {
    api: Arc<Api>,
    path: PathBuf,
}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library").field("path", &self.path).finish_non_exhaustive()
    }
}

impl Library {
    /// Candidate file names, most specific first.
    fn candidates() -> &'static [&'static str] {
        if cfg!(windows) {
            &["libmpv-2.dll", "mpv-2.dll", "mpv-1.dll"]
        } else if cfg!(target_os = "macos") {
            &["libmpv.2.dylib", "libmpv.dylib"]
        } else {
            &["libmpv.so.2", "libmpv.so"]
        }
    }

    /// Load libmpv, preferring a copy next to the executable (how wallSet is
    /// shipped on Windows), then the system search path.
    pub fn load() -> Result<Self, MpvError> {
        // Explicit override, for development and tests (where the executable
        // lives in target/*/deps, nowhere near a shipped DLL).
        if let Some(p) = std::env::var_os("WALLSET_LIBMPV") {
            return Self::load_from(Path::new(&p));
        }

        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
        let mut tried = Vec::new();

        for name in Self::candidates() {
            let mut paths: Vec<PathBuf> = Vec::new();
            if let Some(dir) = &exe_dir {
                paths.push(dir.join(name));
            }
            paths.push(PathBuf::from(name));

            for p in paths {
                tried.push(p.display().to_string());
                // SAFETY: loading libmpv runs its static initialisers, which
                // are well-behaved. We accept that any library on the search
                // path named libmpv is libmpv; the symbol and version checks
                // below catch the impostor case.
                if let Ok(lib) = unsafe { libloading::Library::new(&p) } {
                    return Self::from_library(lib, p);
                }
            }
        }
        Err(MpvError::NotFound { tried: tried.join(", ") })
    }

    /// Load from an explicit path, e.g. a user-configured location.
    pub fn load_from(path: &Path) -> Result<Self, MpvError> {
        // SAFETY: as in `load`.
        let lib = unsafe { libloading::Library::new(path) }
            .map_err(|_| MpvError::NotFound { tried: path.display().to_string() })?;
        Self::from_library(lib, path.to_path_buf())
    }

    fn from_library(lib: libloading::Library, path: PathBuf) -> Result<Self, MpvError> {
        macro_rules! sym {
            ($name:literal) => {{
                // SAFETY: the type is taken from mpv/client.h for this symbol.
                let s = unsafe { lib.get(concat!($name, "\0").as_bytes()) }
                    .map_err(|_| MpvError::MissingSymbol($name))?;
                *s
            }};
        }
        let api = Api {
            client_api_version: sym!("mpv_client_api_version"),
            create: sym!("mpv_create"),
            initialize: sym!("mpv_initialize"),
            terminate_destroy: sym!("mpv_terminate_destroy"),
            set_option_string: sym!("mpv_set_option_string"),
            set_property_string: sym!("mpv_set_property_string"),
            get_property_string: sym!("mpv_get_property_string"),
            command: sym!("mpv_command"),
            wait_event: sym!("mpv_wait_event"),
            free: sym!("mpv_free"),
            error_string: sym!("mpv_error_string"),
            _lib: lib,
        };

        // SAFETY: no arguments, returns a plain integer.
        let v = unsafe { (api.client_api_version)() };
        let (major, minor) = (v >> 16, v & 0xffff);
        if major != REQUIRED_API_MAJOR {
            return Err(MpvError::WrongVersion { found_major: major, found_minor: minor });
        }
        Ok(Self { api: Arc::new(api), path })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `"2.3"` style client API version.
    #[must_use]
    pub fn api_version(&self) -> String {
        // SAFETY: as above.
        let v = unsafe { (self.api.client_api_version)() };
        format!("{}.{}", v >> 16, v & 0xffff)
    }

    /// Create a fresh, uninitialised player. Set options, then call
    /// [`Mpv::initialize`].
    pub fn create(&self) -> Result<Mpv, MpvError> {
        // SAFETY: no preconditions.
        let raw = unsafe { (self.api.create)() };
        let handle = NonNull::new(raw).ok_or(MpvError::CreateFailed)?;
        Ok(Mpv { shared: Arc::new(Handle { api: Arc::clone(&self.api), ptr: handle }) })
    }
}

/// What a waited-for event turned out to be. Only the events wallSet reacts
/// to are distinguished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    None,
    Shutdown,
    FileLoaded,
    VideoReconfig,
    PlaybackRestart,
    EndFile { error: bool, code: c_int },
    Other(c_int),
}

/// The raw handle, destroyed when the last owner ([`Mpv`] or any
/// [`Controller`]) is dropped.
struct Handle {
    api: Arc<Api>,
    ptr: NonNull<MpvHandle>,
}

// SAFETY: libmpv documents every client API function as thread-safe on a
// given handle, with one exception: mpv_wait_event must not be called
// concurrently with itself. Only `Mpv::wait_event` calls it, and it takes
// `&mut self` on a type that cannot be cloned, so exclusivity is enforced by
// the borrow checker.
unsafe impl Send for Handle {}
// SAFETY: as above.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: last reference; destroyed exactly once. Also valid on a
        // handle that was never initialised.
        unsafe { (self.api.terminate_destroy)(self.ptr.as_ptr()) };
    }
}

impl Handle {
    fn check(&self, rc: c_int, context: impl Into<String>) -> Result<(), MpvError> {
        if rc >= 0 {
            return Ok(());
        }
        Err(MpvError::Call { code: rc, context: context.into(), message: self.error_string(rc) })
    }

    fn error_string(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string returns a static string for any code.
        unsafe { CStr::from_ptr((self.api.error_string)(code)) }.to_string_lossy().into_owned()
    }

    fn set_option(&self, name: &str, value: &str) -> Result<(), MpvError> {
        let (n, v) = (CString::new(name)?, CString::new(value)?);
        // SAFETY: valid handle and NUL-terminated strings.
        let rc = unsafe { (self.api.set_option_string)(self.ptr.as_ptr(), n.as_ptr(), v.as_ptr()) };
        self.check(rc, format!("set option {name}={value}"))
    }

    fn set_property(&self, name: &str, value: &str) -> Result<(), MpvError> {
        let (n, v) = (CString::new(name)?, CString::new(value)?);
        // SAFETY: valid handle and NUL-terminated strings.
        let rc =
            unsafe { (self.api.set_property_string)(self.ptr.as_ptr(), n.as_ptr(), v.as_ptr()) };
        self.check(rc, format!("set property {name}={value}"))
    }

    fn get(&self, name: &str) -> Option<String> {
        let n = CString::new(name).ok()?;
        // SAFETY: valid handle; result is null or an mpv-allocated string.
        let raw = unsafe { (self.api.get_property_string)(self.ptr.as_ptr(), n.as_ptr()) };
        if raw.is_null() {
            return None;
        }
        // SAFETY: non-null, NUL-terminated, owned by mpv until mpv_free.
        let s = unsafe { CStr::from_ptr(raw) }.to_string_lossy().into_owned();
        // SAFETY: freeing mpv's own allocation exactly once.
        unsafe { (self.api.free)(raw.cast()) };
        Some(s)
    }

    fn command(&self, args: &[&str]) -> Result<(), MpvError> {
        let owned: Vec<CString> = args.iter().map(|a| CString::new(*a)).collect::<Result<_, _>>()?;
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        // SAFETY: null-terminated array of valid C strings, alive for the call.
        let rc = unsafe { (self.api.command)(self.ptr.as_ptr(), ptrs.as_mut_ptr()) };
        self.check(rc, format!("command {}", args.join(" ")))
    }
}

/// One mpv player instance: the sole owner of the event queue.
pub struct Mpv {
    shared: Arc<Handle>,
}

impl std::fmt::Debug for Mpv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mpv").finish_non_exhaustive()
    }
}

impl Mpv {
    /// Set an option. Before `initialize` this is how almost everything is
    /// configured; afterwards prefer [`Mpv::set_property`].
    pub fn set_option(&mut self, name: &str, value: &str) -> Result<(), MpvError> {
        self.shared.set_option(name, value)
    }

    pub fn initialize(&mut self) -> Result<(), MpvError> {
        // SAFETY: valid handle; called once by convention of this API.
        let rc = unsafe { (self.shared.api.initialize)(self.shared.ptr.as_ptr()) };
        self.shared.check(rc, "initialize")
    }

    pub fn set_property(&self, name: &str, value: &str) -> Result<(), MpvError> {
        self.shared.set_property(name, value)
    }

    /// Read a property as a string. `None` when it is unavailable, which for
    /// many properties simply means "not known yet" or "not applicable".
    #[must_use]
    pub fn get(&self, name: &str) -> Option<String> {
        self.shared.get(name)
    }

    #[must_use]
    pub fn get_f64(&self, name: &str) -> Option<f64> {
        self.get(name)?.trim().parse().ok()
    }

    #[must_use]
    pub fn get_u32(&self, name: &str) -> Option<u32> {
        self.get(name)?.trim().parse().ok()
    }

    /// Run a command, e.g. `["loadfile", path]`.
    pub fn command(&self, args: &[&str]) -> Result<(), MpvError> {
        self.shared.command(args)
    }

    /// A cloneable, thread-safe handle for issuing commands from another
    /// thread while this one blocks in [`Mpv::wait_event`].
    #[must_use]
    pub fn controller(&self) -> Controller {
        Controller { shared: Arc::clone(&self.shared) }
    }

    /// Block for the next event. `None` blocks indefinitely, which is what
    /// the render process does: it sleeps in the kernel rather than spinning.
    pub fn wait_event(&mut self, timeout: Option<Duration>) -> Event {
        let t = timeout.map_or(-1.0, |d| d.as_secs_f64());
        // SAFETY: valid handle; `&mut self` on a non-Clone type guarantees no
        // concurrent waiter. The event struct is valid until the next call.
        let ev = unsafe { &*(self.shared.api.wait_event)(self.shared.ptr.as_ptr(), t) };
        match ev.event_id {
            0 => Event::None,
            1 => Event::Shutdown,
            8 => Event::FileLoaded,
            17 => Event::VideoReconfig,
            21 => Event::PlaybackRestart,
            7 => {
                let (reason, code) = if ev.data.is_null() {
                    (0, 0)
                } else {
                    // SAFETY: for MPV_EVENT_END_FILE, data is mpv_event_end_file.
                    let ef = unsafe { &*(ev.data as *const RawEndFile) };
                    (ef.reason, ef.error)
                };
                // MPV_END_FILE_REASON_ERROR == 4
                Event::EndFile { error: reason == 4, code }
            }
            other => Event::Other(other),
        }
    }

    /// Wait until `pred` matches an event or `total` elapses.
    pub fn wait_for(
        &mut self,
        total: Duration,
        mut pred: impl FnMut(Event) -> bool,
    ) -> Result<Event, MpvError> {
        let deadline = Instant::now() + total;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(MpvError::Timeout(total));
            }
            let ev = self.wait_event(Some(left));
            if let Event::EndFile { error: true, code } = ev {
                return Err(MpvError::LoadFailed(self.shared.error_string(code)));
            }
            if ev == Event::Shutdown || pred(ev) {
                return Ok(ev);
            }
        }
    }
}

/// Thread-safe command handle. See [`Mpv::controller`].
#[derive(Clone)]
pub struct Controller {
    shared: Arc<Handle>,
}

impl std::fmt::Debug for Controller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Controller").finish_non_exhaustive()
    }
}

impl Controller {
    pub fn command(&self, args: &[&str]) -> Result<(), MpvError> {
        self.shared.command(args)
    }

    pub fn set_property(&self, name: &str, value: &str) -> Result<(), MpvError> {
        self.shared.set_property(name, value)
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<String> {
        self.shared.get(name)
    }
}
