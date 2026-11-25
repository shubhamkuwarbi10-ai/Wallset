//! `wallset-daemon`: the resident process.
//!
//! Owns configuration, display topology and one [`PolicyEngine`] per monitor
//! that shows a video. It never decodes and never renders: video lives in
//! short-lived `wallset-render` children that it starts, pauses, freezes and
//! kills as the policy dictates.
//!
//! It never polls either. The thread spends its life blocked in
//! `GetMessageW`; everything that can change a decision arrives as a message:
//!
//! | Source | Message |
//! |---|---|
//! | Window shown, hidden, moved, minimised, cloaked, focused | WinEvent hooks -> debounced `TIMER_REEVAL` |
//! | Grace period expiring | one-shot `TIMER_POLICY`, armed from `next_deadline_ms` |
//! | Monitor on/off, AC/battery, battery level | `WM_POWERBROADCAST` |
//! | Lock / unlock | `WM_WTSSESSION_CHANGE` |
//! | Resolution / monitor added or removed | `WM_DISPLAYCHANGE` -> debounced `TIMER_REAPPLY` |
//! | Explorer restarted | registered `TaskbarCreated` |
//! | Settings saved by the UI | `WM_APP_RELOAD` |
//! | A renderer exited | `WM_APP_CHILD_EXIT`, from a kernel wait |

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod paths;
mod renderer;
mod scan;
mod tray;

use renderer::{Job, Launch, Renderer, WM_APP_CHILD_EXIT};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;
use wallset_core::media::MediaKind;
use wallset_core::policy::Reason;
use wallset_core::{Assignment, Config, Monitor, PolicyEngine, PolicyInputs, Tier};
use wallset_surface::{DesktopHost, HostWindow};
use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{POWERBROADCAST_SETTING, RegisterPowerSettingNotification};
use windows::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification};
use windows::Win32::System::SystemServices::{
    GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING, GUID_CONSOLE_DISPLAY_STATE,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, CreateWindowExW, DefWindowProcW, DispatchMessageW, EVENT_OBJECT_CLOAKED,
    EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_UNCLOAKED,
    EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART,
    EVENT_SYSTEM_MOVESIZEEND, GA_ROOT, GetAncestor, GetMessageW, KillTimer, MSG, OBJID_WINDOW,
    PBT_POWERSETTINGCHANGE, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW,
    SetTimer, TranslateMessage, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP, WM_CONTEXTMENU,
    WM_DESTROY, WM_DISPLAYCHANGE, WM_LBUTTONUP, WM_POWERBROADCAST, WM_RBUTTONUP, WM_TIMER,
    WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

/// Posted by the settings UI after it saves the config.
const WM_APP_RELOAD: u32 = WM_APP + 3;

const TIMER_REEVAL: usize = 1;
const TIMER_POLICY: usize = 2;
const TIMER_REAPPLY: usize = 3;
/// Trailing-edge debounce for window events. A window being dragged emits
/// dozens of events a second; we re-evaluate once it settles.
const REEVAL_DEBOUNCE_MS: u32 = 150;
const REAPPLY_DEBOUNCE_MS: u32 = 750;

const WTS_SESSION_LOCK: usize = 0x7;
const WTS_SESSION_UNLOCK: usize = 0x8;

#[derive(Debug, Clone, Copy)]
struct Signals {
    display_on: bool,
    locked: bool,
    on_battery: bool,
    battery_pct: Option<u8>,
    user_paused: bool,
}

/// One monitor showing a video.
struct Slot {
    monitor: Monitor,
    assignment: Assignment,
    engine: PolicyEngine,
    window: HostWindow,
    renderer: Option<Renderer>,
    freezing: bool,
    resume_pos: Option<f64>,
    freeze_png: PathBuf,
    crashes: Vec<Instant>,
}

struct Daemon {
    hwnd: HWND,
    cfg: Config,
    host: Option<DesktopHost>,
    slots: Vec<Slot>,
    signals: Signals,
    job: Job,
    t0: Instant,
    render_exe: PathBuf,
    adapter: Option<String>,
    taskbar_created: u32,
}

thread_local! {
    static DAEMON: RefCell<Option<Daemon>> = const { RefCell::new(None) };
}

fn main() {
    wallset_surface::process::init_dpi_awareness();

    // One daemon per session. A second launch asks the first to reload.
    // SAFETY: named mutex in the session namespace; intentionally leaked.
    let _mutex = unsafe { CreateMutexW(None, true, w!(r"Local\wallSet.Daemon")) };
    // SAFETY: reads this thread's last error, set by CreateMutexW.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if let Ok(h) = unsafe { windows::Win32::UI::WindowsAndMessaging::FindWindowW(w!("wallSet.Daemon"), PCWSTR::null()) } {
            // SAFETY: posting to the other instance's window.
            unsafe {
                let _ = PostMessageW(Some(h), WM_APP_RELOAD, WPARAM(0), LPARAM(0));
            }
        }
        return;
    }

    wallset_surface::process::enter_background_mode();
    paths::ensure_dirs();
    paths::rotate_log();
    paths::log("daemon starting");

    let hwnd = match create_message_window() {
        Ok(h) => h,
        Err(e) => {
            paths::log(&format!("cannot create window: {e}"));
            return;
        }
    };
    renderer::NOTIFY_HWND.store(hwnd.0 as isize, Ordering::Release);

    register_notifications(hwnd);
    let hooks = install_hooks();
    tray::add(hwnd, "wallSet");

    let job = match Job::new() {
        Ok(j) => j,
        Err(e) => {
            paths::log(&format!("cannot create job object: {e}"));
            return;
        }
    };
    let render_exe = std::env::current_exe()
        .map(|p| p.with_file_name("wallset-render.exe"))
        .unwrap_or_default();

    let cfg = paths::load_config();
    let adapter = cfg.render.prefer_power_saving_gpu.then(wallset_display::power_saving_adapter).flatten();
    paths::sync_autostart(cfg.start_with_windows);

    DAEMON.with(|d| {
        *d.borrow_mut() = Some(Daemon {
            hwnd,
            cfg,
            host: None,
            slots: Vec::new(),
            signals: Signals { display_on: true, locked: false, on_battery: false, battery_pct: None, user_paused: false },
            job,
            t0: Instant::now(),
            render_exe,
            adapter,
            // SAFETY: plain registration of a well-known string.
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        });
    });
    with_daemon(Daemon::apply_config);

    let mut msg = MSG::default();
    // SAFETY: standard message loop. This is where the daemon spends ~all of
    // its life, blocked in the kernel at zero CPU.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    for h in hooks {
        // SAFETY: hooks we installed.
        unsafe {
            let _ = UnhookWinEvent(h);
        }
    }
    tray::remove(hwnd);
    // Dropping the daemon drops slots (killing renderers) and then the job.
    DAEMON.with(|d| d.borrow_mut().take());
    paths::log("daemon stopped");
}

/// Run `f` against the daemon unless the state is already borrowed, which
/// happens when a nested message loop (a tray menu, a cross-thread
/// SendMessage) re-enters the window procedure. In that case the caller
/// re-posts its message so nothing is lost.
fn with_daemon<R>(f: impl FnOnce(&mut Daemon) -> R) -> Option<R> {
    DAEMON.with(|d| d.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

impl Daemon {
    fn now_ms(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }

    /// Rebuild everything from config: enumerate monitors, find the desktop
    /// layer, set still images, create video slots. Used at startup, after a
    /// display change, after Explorer restarts and after a settings change.
    fn apply_config(&mut self) {
        self.slots.clear(); // kills renderers, destroys host windows

        let monitors = match wallset_display::enumerate() {
            Ok(m) => m,
            Err(e) => {
                paths::log(&format!("display enumeration failed: {e}"));
                return;
            }
        };
        let needs_host = self.cfg.assignments.iter().any(|a| a.kind.needs_renderer());
        self.host = if needs_host {
            DesktopHost::acquire()
                .inspect_err(|e| paths::log(&format!("video unavailable: {e}")))
                .ok()
        } else {
            None
        };

        for a in self.cfg.assignments.clone() {
            let Some(m) = monitors.iter().find(|m| m.id == a.monitor) else {
                paths::log(&format!("monitor {} not connected; skipping", a.monitor));
                continue;
            };
            if a.kind == MediaKind::Image {
                match wallset_surface::set_static_wallpaper(m.bounds, &a.path) {
                    Ok(()) => paths::log(&format!("{}: still image {}", m.friendly_name, a.path.display())),
                    Err(e) => paths::log(&format!("{}: cannot set image: {e}", m.friendly_name)),
                }
                continue;
            }
            let Some(host) = &self.host else { continue };
            match host.create_window(m.bounds) {
                Ok(window) => self.slots.push(Slot {
                    freeze_png: paths::freeze_frame(&m.id.0),
                    monitor: m.clone(),
                    assignment: a,
                    engine: PolicyEngine::new(self.cfg.policy),
                    window,
                    renderer: None,
                    freezing: false,
                    resume_pos: None,
                    crashes: Vec::new(),
                }),
                Err(e) => paths::log(&format!("{}: cannot create host window: {e}", m.friendly_name)),
            }
        }
        paths::log(&format!(
            "applied: {} monitor(s), {} video slot(s), layout {:?}, render GPU {}",
            monitors.len(),
            self.slots.len(),
            self.host.as_ref().map(DesktopHost::layout),
            self.adapter.as_deref().unwrap_or("system default")
        ));
        self.reevaluate();
    }

    /// Feed current signals to every slot's policy engine, then make the
    /// renderers match the decided tiers.
    fn reevaluate(&mut self) {
        if self.slots.is_empty() {
            self.arm_policy_timer();
            return;
        }
        let rects: Vec<_> = self.slots.iter().map(|s| s.monitor.bounds).collect();
        let sc = scan::scan(&rects);
        let now = self.now_ms();
        let sig = self.signals;

        for (i, slot) in self.slots.iter_mut().enumerate() {
            let inputs = PolicyInputs {
                desktop_coverage: sc.coverage[i],
                fullscreen_app: sc.fullscreen[i],
                monitor_powered: sig.display_on,
                session_locked: sig.locked,
                on_battery: sig.on_battery,
                battery_percent: sig.battery_pct,
                user_paused: sig.user_paused,
            };
            if let Some(t) = slot.engine.update(&inputs, now) {
                paths::log(&format!(
                    "{}: {:?} -> {:?} ({})",
                    slot.monitor.friendly_name,
                    t.from,
                    t.to,
                    t.reason.explain()
                ));
            }
        }
        let indices: Vec<usize> = (0..self.slots.len()).collect();
        for i in indices {
            self.reconcile(i);
        }
        self.arm_policy_timer();
        self.update_tray_tip();
    }

    /// Make slot `i`'s renderer match its tier. Idempotent.
    fn reconcile(&mut self, i: usize) {
        let tier = self.slots[i].engine.tier();
        let slot = &mut self.slots[i];
        if slot.freezing {
            return; // wait for the exit notification
        }
        match (tier, slot.renderer.as_mut()) {
            (Tier::Visible, None) => self.spawn(i),
            (Tier::Visible, Some(r)) => r.resume(),
            // Paused costs CPU nothing. Not spawning a renderer just to hold
            // it paused and invisible: the freeze frame is already showing.
            (Tier::Occluded, Some(r)) => r.pause(),
            (Tier::Occluded, None) | (Tier::Suspended, None) => {}
            (Tier::Suspended, Some(r)) => {
                let png = slot.freeze_png.clone();
                let _ = std::fs::remove_file(&png);
                r.send(&format!("freeze {}", png.display()));
                slot.freezing = true;
            }
        }
    }

    fn spawn(&mut self, i: usize) {
        let slot = &self.slots[i];
        let stderr_log = paths::local_dir().join("render.log");
        let launch = Launch {
            exe: &self.render_exe,
            wid: slot.window.raw(),
            file: &slot.assignment.path,
            start_secs: slot.resume_pos,
            args: self.cfg.render.renderer_args(slot.assignment.fit, self.adapter.as_deref()),
            stderr_log: Some(&stderr_log),
        };
        match Renderer::spawn(&launch, &self.job) {
            Ok(r) => {
                paths::log(&format!("{}: renderer pid {} started", slot.monitor.friendly_name, r.pid()));
                self.slots[i].renderer = Some(r);
            }
            Err(e) => paths::log(&format!("{}: cannot start renderer: {e}", slot.monitor.friendly_name)),
        }
    }

    fn on_child_exit(&mut self, pid: u32) {
        let Some(i) = self.slots.iter().position(|s| s.renderer.as_ref().is_some_and(|r| r.pid() == pid)) else {
            return; // a renderer we already dropped
        };
        let slot = &mut self.slots[i];
        slot.renderer = None; // reaps the process, unregisters the wait

        if slot.freezing {
            slot.freezing = false;
            if slot.freeze_png.exists() {
                slot.resume_pos = std::fs::read_to_string(format!("{}.pos", slot.freeze_png.display()))
                    .ok()
                    .and_then(|s| s.trim().parse().ok());
                if let Err(e) = wallset_surface::set_static_wallpaper(slot.monitor.bounds, &slot.freeze_png) {
                    paths::log(&format!("{}: cannot set freeze frame: {e}", slot.monitor.friendly_name));
                }
            }
            slot.window.hide();
            paths::log(&format!("{}: renderer frozen and released", slot.monitor.friendly_name));
        } else {
            // Unexpected exit: a crash, a driver reset, a file that went away.
            slot.window.hide();
            let now = Instant::now();
            slot.crashes.retain(|t| now.duration_since(*t).as_secs() < 60);
            slot.crashes.push(now);
            paths::log(&format!(
                "{}: renderer exited unexpectedly ({} in the last minute)",
                slot.monitor.friendly_name,
                slot.crashes.len()
            ));
            if slot.crashes.len() >= 3 {
                paths::log(&format!("{}: giving up on this video until settings change", slot.monitor.friendly_name));
                return;
            }
        }
        // The world may have changed while the renderer was going away.
        self.reconcile(i);
    }

    fn arm_policy_timer(&self) {
        let now = self.now_ms();
        let next = self.slots.iter().filter_map(|s| s.engine.next_deadline_ms()).min();
        // SAFETY: timers on our own window.
        unsafe {
            match next {
                Some(at) => {
                    SetTimer(Some(self.hwnd), TIMER_POLICY, at.saturating_sub(now).clamp(10, u64::from(u32::MAX)) as u32, None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), TIMER_POLICY);
                }
            }
        }
    }

    fn update_tray_tip(&self) {
        let playing = self.slots.iter().filter(|s| s.engine.tier() == Tier::Visible).count();
        let tip = if self.signals.user_paused {
            "wallSet: paused".to_string()
        } else if self.slots.is_empty() {
            "wallSet".to_string()
        } else {
            let why = self
                .slots
                .iter()
                .find(|s| s.engine.tier() != Tier::Visible)
                .map(|s| s.engine.reason())
                .filter(|r| *r != Reason::DesktopVisible);
            match why {
                Some(r) if playing == 0 => format!("wallSet: idle, {}", r.explain()),
                _ => format!("wallSet: {playing} of {} playing", self.slots.len()),
            }
        };
        tray::set_tip(self.hwnd, &tip);
    }

    fn on_power(&mut self, lparam: LPARAM) {
        // SAFETY: for PBT_POWERSETTINGCHANGE, lparam points at a
        // POWERBROADCAST_SETTING whose Data holds at least a DWORD.
        let s = unsafe { &*(lparam.0 as *const POWERBROADCAST_SETTING) };
        let value = unsafe { std::ptr::read_unaligned(s.Data.as_ptr().cast::<u32>()) };
        if s.PowerSetting == GUID_CONSOLE_DISPLAY_STATE {
            self.signals.display_on = value != 0; // 0 off, 1 on, 2 dimmed
        } else if s.PowerSetting == GUID_ACDC_POWER_SOURCE {
            self.signals.on_battery = value != 0; // 0 AC, 1 DC, 2 short-term
        } else if s.PowerSetting == GUID_BATTERY_PERCENTAGE_REMAINING {
            self.signals.battery_pct = Some(value.min(100) as u8);
        }
        self.reevaluate();
    }

    fn open_settings(&self) {
        let ui = self.render_exe.with_file_name("wallset-ui.exe");
        if let Err(e) = std::process::Command::new(&ui).spawn() {
            paths::log(&format!("cannot open settings ({}): {e}", ui.display()));
        }
    }
}

fn create_message_window() -> windows::core::Result<HWND> {
    // SAFETY: class registration and creation of our own hidden window. It is
    // a real top-level window (not HWND_MESSAGE) because WM_DISPLAYCHANGE and
    // TaskbarCreated are only broadcast to top-level windows.
    unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null())?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: w!("wallSet.Daemon"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("wallSet.Daemon"),
            w!("wallSet"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
    }
}

fn register_notifications(hwnd: HWND) {
    let h = HANDLE(hwnd.0);
    for guid in [&GUID_CONSOLE_DISPLAY_STATE, &GUID_ACDC_POWER_SOURCE, &GUID_BATTERY_PERCENTAGE_REMAINING] {
        // SAFETY: registering our window; Windows sends the current value
        // immediately, so initial state needs no query.
        let _ = unsafe {
            RegisterPowerSettingNotification(h, guid, windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE)
        };
    }
    // SAFETY: our window, this session.
    let _ = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) };
}

/// Each hook covers a narrow event range on purpose: one hook spanning
/// 0x8001..0x8018 would also deliver NAMECHANGE and VALUECHANGE, which fire
/// constantly (every caret move, every progress bar tick).
fn install_hooks() -> Vec<HWINEVENTHOOK> {
    let ranges = [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
        (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
        (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    ];
    ranges
        .into_iter()
        .map(|(lo, hi)| {
            // SAFETY: out-of-context hook; the callback runs on this thread
            // during message retrieval.
            unsafe { SetWinEventHook(lo, hi, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS) }
        })
        .filter(|h| !h.is_invalid())
        .collect()
}

unsafe extern "system" fn win_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only whole top-level windows. This discards the caret, scroll bars,
    // and every child control's location changes.
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 || hwnd.is_invalid() {
        return;
    }
    // SAFETY: plain query on any handle.
    if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
        return;
    }
    let d = renderer::NOTIFY_HWND.load(Ordering::Acquire);
    // Re-arming the same timer ID restarts it: a trailing-edge debounce.
    // SAFETY: timer on our own window.
    unsafe {
        SetTimer(Some(HWND(d as *mut _)), TIMER_REEVAL, REEVAL_DEBOUNCE_MS, None);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let repost = || unsafe {
        let _ = PostMessageW(Some(hwnd), msg, wp, lp);
    };
    match msg {
        WM_TIMER => {
            // SAFETY: our timers are one-shot by convention.
            unsafe {
                let _ = KillTimer(Some(hwnd), wp.0);
            }
            let ran = match wp.0 {
                TIMER_REEVAL | TIMER_POLICY => with_daemon(Daemon::reevaluate),
                TIMER_REAPPLY => with_daemon(Daemon::apply_config),
                _ => Some(()),
            };
            if ran.is_none() {
                // Re-entered; try again shortly.
                unsafe {
                    SetTimer(Some(hwnd), wp.0, 50, None);
                }
            }
            LRESULT(0)
        }
        WM_APP_CHILD_EXIT => {
            if with_daemon(|d| d.on_child_exit(wp.0 as u32)).is_none() {
                repost();
            }
            LRESULT(0)
        }
        WM_APP_RELOAD => {
            let ok = with_daemon(|d| {
                d.cfg = paths::load_config();
                d.adapter = d.cfg.render.prefer_power_saving_gpu.then(wallset_display::power_saving_adapter).flatten();
                paths::sync_autostart(d.cfg.start_with_windows);
                paths::log("settings reloaded");
                d.apply_config();
            });
            if ok.is_none() {
                repost();
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            // Display changes arrive in bursts; rebuild once they stop.
            unsafe {
                SetTimer(Some(hwnd), TIMER_REAPPLY, REAPPLY_DEBOUNCE_MS, None);
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST if wp.0 == PBT_POWERSETTINGCHANGE as usize => {
            // lparam is only valid during this call, so it cannot be reposted.
            let _ = with_daemon(|d| d.on_power(lp));
            LRESULT(1)
        }
        WM_WTSSESSION_CHANGE => {
            let locked = match wp.0 {
                WTS_SESSION_LOCK => Some(true),
                WTS_SESSION_UNLOCK => Some(false),
                _ => None,
            };
            if let Some(l) = locked
                && with_daemon(|d| {
                    d.signals.locked = l;
                    d.reevaluate();
                })
                .is_none()
            {
                repost();
            }
            LRESULT(0)
        }
        tray::WM_APP_TRAY => {
            let event = (lp.0 & 0xffff) as u32;
            if matches!(event, WM_RBUTTONUP | WM_LBUTTONUP | WM_CONTEXTMENU) {
                // Read state, release the borrow, *then* run the modal menu.
                let paused = with_daemon(|d| d.signals.user_paused).unwrap_or(false);
                match tray::menu(hwnd, paused) {
                    Some(tray::CMD_SETTINGS) => {
                        let _ = with_daemon(|d| d.open_settings());
                    }
                    Some(tray::CMD_PAUSE) => {
                        let _ = with_daemon(|d| {
                            d.signals.user_paused = !d.signals.user_paused;
                            d.reevaluate();
                        });
                    }
                    Some(tray::CMD_QUIT) => unsafe { PostQuitMessage(0) },
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        m if m != 0 && DAEMON.with(|d| d.try_borrow().ok().and_then(|g| g.as_ref().map(|d| d.taskbar_created))) == Some(m) => {
            // Explorer restarted: every desktop handle we held is dead.
            let _ = with_daemon(|d| {
                tray::add(d.hwnd, "wallSet");
                paths::log("Explorer restarted; re-acquiring the desktop");
                d.apply_config();
            });
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
