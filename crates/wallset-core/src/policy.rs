//! The resource policy engine.
//!
//! This is the component that turns "lightweight" from a claim into a
//! mechanism. It decides, from push-based OS signals, whether the video
//! renderer should be **playing**, **paused** or **killed outright**.
//!
//! Design notes worth keeping in mind when editing:
//!
//! * Time is passed in as monotonic milliseconds rather than read from a
//!   clock, so the whole state machine is testable without sleeping.
//! * The engine never polls. It exposes [`PolicyEngine::next_deadline_ms`] so
//!   the host can arm a single one-shot timer and otherwise stay blocked in
//!   the OS event loop.
//! * Occlusion uses hysteresis. A window dragged to hover exactly at the
//!   threshold must not make us start and stop the decoder dozens of times a
//!   second — that would cost more than just leaving it running.

use serde::{Deserialize, Serialize};

/// What the renderer should be doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    /// Desktop is reachable by the user: decode and present.
    Visible,
    /// Hidden for the moment: pause decoding, keep the process warm so that
    /// resuming is instant. CPU goes to zero; RAM does not.
    Occluded,
    /// Hidden for long enough, or in a state where resources matter more than
    /// latency: terminate the renderer entirely. RAM and CPU both go to zero.
    /// The last decoded frame remains as a static desktop image, so the user
    /// never sees a black desktop.
    Suspended,
}

impl Tier {
    /// Whether a renderer process should exist at all in this tier.
    #[must_use]
    pub const fn needs_render_process(self) -> bool {
        matches!(self, Self::Visible | Self::Occluded)
    }
}

/// Why the engine chose the tier it did. Surfaced in the UI and the log, so
/// that "my wallpaper stopped" is self-diagnosing rather than a support ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reason {
    DesktopVisible,
    StaticWallpaper,
    MonitorPoweredOff,
    FullscreenApp,
    BatteryLow,
    OnBattery,
    SessionLocked,
    Occluded,
    OccludedTooLong,
    LockedTooLong,
    UserPaused,
}

impl Reason {
    #[must_use]
    pub const fn explain(self) -> &'static str {
        match self {
            Self::DesktopVisible => "desktop is visible",
            Self::StaticWallpaper => "wallpaper is a still image; no renderer needed",
            Self::MonitorPoweredOff => "monitor is powered off",
            Self::FullscreenApp => "a fullscreen application is in the foreground",
            Self::BatteryLow => "running on battery below the configured threshold",
            Self::OnBattery => "running on battery",
            Self::SessionLocked => "session is locked",
            Self::Occluded => "the desktop is covered by other windows",
            Self::OccludedTooLong => "the desktop has been covered long enough to reclaim memory",
            Self::LockedTooLong => "the session has been locked long enough to reclaim memory",
            Self::UserPaused => "video wallpapers are paused from the tray menu",
        }
    }
}

/// Push-based signals gathered from the OS. Every field corresponds to an
/// event source listed in `docs/ARCHITECTURE.md` section 3 — none of them is
/// obtained by polling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolicyInputs {
    /// Fraction of the monitor covered by non-desktop windows, `0.0..=1.0`.
    pub desktop_coverage: f64,
    /// A fullscreen exclusive or presentation-mode app is running.
    pub fullscreen_app: bool,
    /// The monitor is actually powered on.
    pub monitor_powered: bool,
    pub session_locked: bool,
    pub on_battery: bool,
    pub battery_percent: Option<u8>,
    /// False when the active wallpaper is a still image, which needs no
    /// renderer at all.
    pub media_is_video: bool,
    /// The user chose "Pause video wallpapers" in the tray menu.
    pub user_paused: bool,
}

impl Default for PolicyInputs {
    fn default() -> Self {
        Self {
            desktop_coverage: 0.0,
            fullscreen_app: false,
            monitor_powered: true,
            session_locked: false,
            on_battery: false,
            battery_percent: None,
            media_is_video: true,
            user_paused: false,
        }
    }
}

/// User-tunable policy. Defaults are chosen to be unsurprising rather than
/// maximally aggressive: a wallpaper that stops playing while you are looking
/// at it is a worse bug than one that wastes 2% CPU.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    /// Coverage at or above which the desktop counts as hidden.
    /// Not 1.0: a maximised window leaves a pixel or two of rounding error,
    /// and on Windows 11 rounded corners mean a "maximised" window genuinely
    /// never covers 100%.
    pub occlusion_enter: f64,
    /// Coverage below which it counts as visible again. Strictly lower than
    /// `occlusion_enter` to give hysteresis.
    pub occlusion_exit: f64,
    /// How long the desktop must stay covered before we escalate from pause
    /// to kill.
    pub occluded_grace_ms: u64,
    /// How long the session must stay locked before we escalate.
    pub lock_grace_ms: u64,
    /// Kill the renderer immediately whenever on battery, regardless of level.
    pub suspend_on_battery_always: bool,
    /// Kill the renderer when on battery at or below this percentage.
    pub battery_threshold_pct: u8,
    /// When false, the Occluded tier is skipped: hidden means keep playing
    /// until the grace period kills it. Exists for users who would rather have
    /// instant resume than zero CPU.
    pub pause_when_occluded: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            occlusion_enter: 0.95,
            occlusion_exit: 0.90,
            occluded_grace_ms: 90_000,
            lock_grace_ms: 60_000,
            suspend_on_battery_always: false,
            battery_threshold_pct: 30,
            pause_when_occluded: true,
        }
    }
}

impl PolicyConfig {
    /// Clamp a deserialised config into a sane range. A config file edited by
    /// hand must not be able to put the engine into a flapping state.
    pub fn sanitise(&mut self) {
        self.occlusion_enter = self.occlusion_enter.clamp(0.50, 1.0);
        self.occlusion_exit = self.occlusion_exit.clamp(0.0, self.occlusion_enter - 0.01);
        self.occluded_grace_ms = self.occluded_grace_ms.clamp(1_000, 3_600_000);
        self.lock_grace_ms = self.lock_grace_ms.clamp(1_000, 3_600_000);
        self.battery_threshold_pct = self.battery_threshold_pct.min(100);
    }
}

/// Emitted when the tier changes. The host acts on this and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: Tier,
    pub to: Tier,
    pub reason: Reason,
}

/// The state machine itself.
#[derive(Debug, Clone)]
pub struct PolicyEngine {
    cfg: PolicyConfig,
    tier: Tier,
    reason: Reason,
    /// Set to the timestamp at which occlusion began, cleared when it ends.
    occluded_since_ms: Option<u64>,
    locked_since_ms: Option<u64>,
    /// Sticky occlusion flag implementing the hysteresis band.
    occluded_latched: bool,
}

impl PolicyEngine {
    #[must_use]
    pub fn new(mut cfg: PolicyConfig) -> Self {
        cfg.sanitise();
        Self {
            cfg,
            tier: Tier::Visible,
            reason: Reason::DesktopVisible,
            occluded_since_ms: None,
            locked_since_ms: None,
            occluded_latched: false,
        }
    }

    #[must_use]
    pub const fn tier(&self) -> Tier {
        self.tier
    }

    #[must_use]
    pub const fn reason(&self) -> Reason {
        self.reason
    }

    #[must_use]
    pub const fn config(&self) -> &PolicyConfig {
        &self.cfg
    }

    pub fn set_config(&mut self, mut cfg: PolicyConfig) {
        cfg.sanitise();
        self.cfg = cfg;
    }

    /// Feed the current OS signals in and get back a transition, if any.
    ///
    /// Returns `None` when nothing changed, which is the common case — the
    /// host then does no work at all.
    pub fn update(&mut self, inputs: &PolicyInputs, now_ms: u64) -> Option<Transition> {
        let (target, reason) = self.evaluate(inputs, now_ms);
        let changed = target != self.tier;
        let prev = self.tier;
        self.tier = target;
        self.reason = reason;
        changed.then_some(Transition { from: prev, to: target, reason })
    }

    /// The single moment in the future at which the tier could change without
    /// any new OS event. The host arms one one-shot timer for this and
    /// otherwise stays blocked.
    ///
    /// `None` means "nothing pending; sleep until the OS wakes you".
    #[must_use]
    pub fn next_deadline_ms(&self) -> Option<u64> {
        if self.tier == Tier::Suspended {
            return None;
        }
        let occ = self.occluded_since_ms.map(|t| t.saturating_add(self.cfg.occluded_grace_ms));
        let lock = self.locked_since_ms.map(|t| t.saturating_add(self.cfg.lock_grace_ms));
        match (occ, lock) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn evaluate(&mut self, i: &PolicyInputs, now_ms: u64) -> (Tier, Reason) {
        // A still image needs no renderer in any state.
        if !i.media_is_video {
            self.clear_timers();
            return (Tier::Suspended, Reason::StaticWallpaper);
        }

        // Hard suspends: reclaim everything immediately, no grace period.
        if i.user_paused {
            self.clear_timers();
            return (Tier::Suspended, Reason::UserPaused);
        }
        if !i.monitor_powered {
            self.clear_timers();
            return (Tier::Suspended, Reason::MonitorPoweredOff);
        }
        if i.fullscreen_app {
            self.clear_timers();
            return (Tier::Suspended, Reason::FullscreenApp);
        }
        if i.on_battery {
            if self.cfg.suspend_on_battery_always {
                self.clear_timers();
                return (Tier::Suspended, Reason::OnBattery);
            }
            if i.battery_percent.is_some_and(|p| p <= self.cfg.battery_threshold_pct) {
                self.clear_timers();
                return (Tier::Suspended, Reason::BatteryLow);
            }
        }

        // Soft conditions, each with its own grace timer.
        let occluded = self.update_occlusion_latch(i.desktop_coverage);

        if occluded {
            self.occluded_since_ms.get_or_insert(now_ms);
        } else {
            self.occluded_since_ms = None;
        }
        if i.session_locked {
            self.locked_since_ms.get_or_insert(now_ms);
        } else {
            self.locked_since_ms = None;
        }

        if !occluded && !i.session_locked {
            return (Tier::Visible, Reason::DesktopVisible);
        }

        // Escalate to Suspended once either grace period has elapsed.
        if self
            .locked_since_ms
            .is_some_and(|t| now_ms.saturating_sub(t) >= self.cfg.lock_grace_ms)
        {
            return (Tier::Suspended, Reason::LockedTooLong);
        }
        if self
            .occluded_since_ms
            .is_some_and(|t| now_ms.saturating_sub(t) >= self.cfg.occluded_grace_ms)
        {
            return (Tier::Suspended, Reason::OccludedTooLong);
        }

        let reason = if i.session_locked { Reason::SessionLocked } else { Reason::Occluded };
        if self.cfg.pause_when_occluded {
            (Tier::Occluded, reason)
        } else {
            (Tier::Visible, reason)
        }
    }

    /// Schmitt-trigger on coverage, to stop a window hovering at the threshold
    /// from making us start and stop the decoder repeatedly.
    fn update_occlusion_latch(&mut self, coverage: f64) -> bool {
        if self.occluded_latched {
            if coverage < self.cfg.occlusion_exit {
                self.occluded_latched = false;
            }
        } else if coverage >= self.cfg.occlusion_enter {
            self.occluded_latched = true;
        }
        self.occluded_latched
    }

    fn clear_timers(&mut self) {
        self.occluded_since_ms = None;
        self.locked_since_ms = None;
        self.occluded_latched = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> PolicyEngine {
        PolicyEngine::new(PolicyConfig::default())
    }

    #[test]
    fn visible_desktop_plays() {
        let mut e = engine();
        let t = e.update(&PolicyInputs::default(), 0);
        assert_eq!(t, None, "already Visible, so no transition is emitted");
        assert_eq!(e.tier(), Tier::Visible);
        assert_eq!(e.next_deadline_ms(), None, "nothing pending means no timer");
    }

    #[test]
    fn static_wallpaper_never_spawns_a_renderer() {
        let mut e = engine();
        let i = PolicyInputs { media_is_video: false, ..Default::default() };
        e.update(&i, 0);
        assert_eq!(e.tier(), Tier::Suspended);
        assert!(!e.tier().needs_render_process());
        assert_eq!(e.reason(), Reason::StaticWallpaper);
    }

    #[test]
    fn fullscreen_app_suspends_immediately_without_grace() {
        let mut e = engine();
        let i = PolicyInputs { fullscreen_app: true, ..Default::default() };
        let t = e.update(&i, 0).expect("tier changed");
        assert_eq!(t.to, Tier::Suspended);
        assert_eq!(t.reason, Reason::FullscreenApp);
        assert_eq!(e.next_deadline_ms(), None, "Suspended is terminal until inputs change");
    }

    #[test]
    fn user_pause_suspends_and_unpause_resumes() {
        let mut e = engine();
        let paused = PolicyInputs { user_paused: true, ..Default::default() };
        let t = e.update(&paused, 0).unwrap();
        assert_eq!((t.to, t.reason), (Tier::Suspended, Reason::UserPaused));
        assert_eq!(e.update(&PolicyInputs::default(), 1).unwrap().to, Tier::Visible);
    }

    #[test]
    fn monitor_power_off_suspends_immediately() {
        let mut e = engine();
        let i = PolicyInputs { monitor_powered: false, ..Default::default() };
        assert_eq!(e.update(&i, 0).unwrap().to, Tier::Suspended);
    }

    #[test]
    fn occlusion_pauses_then_escalates_to_suspend_after_grace() {
        let mut e = engine();
        let covered = PolicyInputs { desktop_coverage: 1.0, ..Default::default() };

        let t = e.update(&covered, 1_000).expect("Visible -> Occluded");
        assert_eq!(t.to, Tier::Occluded);
        assert_eq!(
            e.next_deadline_ms(),
            Some(1_000 + 90_000),
            "host should arm exactly one timer for the escalation"
        );

        // One millisecond before the grace expires: still merely paused.
        assert_eq!(e.update(&covered, 90_999), None);
        assert_eq!(e.tier(), Tier::Occluded);

        // At the deadline: memory is reclaimed.
        let t = e.update(&covered, 91_000).expect("Occluded -> Suspended");
        assert_eq!(t.to, Tier::Suspended);
        assert_eq!(t.reason, Reason::OccludedTooLong);
    }

    #[test]
    fn uncovering_the_desktop_resumes_and_resets_the_grace_timer() {
        let mut e = engine();
        let covered = PolicyInputs { desktop_coverage: 1.0, ..Default::default() };
        let clear = PolicyInputs::default();

        e.update(&covered, 0);
        e.update(&covered, 50_000);
        assert_eq!(e.tier(), Tier::Occluded);

        let t = e.update(&clear, 60_000).expect("Occluded -> Visible");
        assert_eq!(t.to, Tier::Visible);
        assert_eq!(e.next_deadline_ms(), None);

        // Re-covering restarts the full 90 s, it does not resume at 50 s.
        e.update(&covered, 70_000);
        assert_eq!(e.next_deadline_ms(), Some(70_000 + 90_000));
    }

    #[test]
    fn coverage_hysteresis_prevents_flapping_at_the_threshold() {
        let mut e = engine();
        let mut i = PolicyInputs::default();

        // Cross the enter threshold.
        i.desktop_coverage = 0.96;
        assert_eq!(e.update(&i, 0).unwrap().to, Tier::Occluded);

        // Dip into the hysteresis band: must stay Occluded, not flap back.
        i.desktop_coverage = 0.92;
        assert_eq!(e.update(&i, 10).or(e.update(&i, 20)), None);
        assert_eq!(e.tier(), Tier::Occluded, "0.92 is inside the band, stay latched");

        // Drop below the exit threshold: now we genuinely resume.
        i.desktop_coverage = 0.80;
        assert_eq!(e.update(&i, 30).unwrap().to, Tier::Visible);
    }

    #[test]
    fn battery_threshold_suspends_but_only_on_battery() {
        let mut e = engine();

        // Low battery while plugged in is irrelevant.
        let plugged = PolicyInputs {
            on_battery: false,
            battery_percent: Some(5),
            ..Default::default()
        };
        e.update(&plugged, 0);
        assert_eq!(e.tier(), Tier::Visible);

        let unplugged =
            PolicyInputs { on_battery: true, battery_percent: Some(25), ..Default::default() };
        let t = e.update(&unplugged, 1).expect("tier changed");
        assert_eq!(t.to, Tier::Suspended);
        assert_eq!(t.reason, Reason::BatteryLow);
    }

    #[test]
    fn healthy_battery_keeps_playing_by_default() {
        let mut e = engine();
        let i = PolicyInputs { on_battery: true, battery_percent: Some(80), ..Default::default() };
        e.update(&i, 0);
        assert_eq!(e.tier(), Tier::Visible, "default policy is not aggressive about battery");
    }

    #[test]
    fn lock_grace_is_shorter_than_occlusion_grace() {
        let mut e = engine();
        let locked = PolicyInputs { session_locked: true, ..Default::default() };

        e.update(&locked, 0);
        assert_eq!(e.tier(), Tier::Occluded);
        assert_eq!(e.next_deadline_ms(), Some(60_000));

        let t = e.update(&locked, 60_000).expect("escalates");
        assert_eq!(t.to, Tier::Suspended);
        assert_eq!(t.reason, Reason::LockedTooLong);
    }

    #[test]
    fn simultaneous_conditions_use_the_earliest_deadline() {
        let mut e = engine();
        let both = PolicyInputs {
            session_locked: true,
            desktop_coverage: 1.0,
            ..Default::default()
        };
        e.update(&both, 0);
        assert_eq!(e.next_deadline_ms(), Some(60_000), "lock grace is the earlier of the two");
    }

    #[test]
    fn pause_when_occluded_disabled_keeps_playing_until_the_kill() {
        let cfg = PolicyConfig { pause_when_occluded: false, ..Default::default() };
        let mut e = PolicyEngine::new(cfg);
        let covered = PolicyInputs { desktop_coverage: 1.0, ..Default::default() };

        e.update(&covered, 0);
        assert_eq!(e.tier(), Tier::Visible, "user opted for instant resume over zero CPU");
        // The escalation timer still runs.
        assert_eq!(e.next_deadline_ms(), Some(90_000));
        assert_eq!(e.update(&covered, 90_000).unwrap().to, Tier::Suspended);
    }

    #[test]
    fn hand_edited_config_cannot_create_an_inverted_hysteresis_band() {
        let mut cfg = PolicyConfig { occlusion_enter: 0.5, occlusion_exit: 0.9, ..Default::default() };
        cfg.sanitise();
        assert!(
            cfg.occlusion_exit < cfg.occlusion_enter,
            "exit {} must stay below enter {}",
            cfg.occlusion_exit,
            cfg.occlusion_enter
        );
    }

    #[test]
    fn zero_grace_in_a_config_file_is_clamped_not_honoured() {
        let mut cfg = PolicyConfig { occluded_grace_ms: 0, lock_grace_ms: 0, ..Default::default() };
        cfg.sanitise();
        assert_eq!(cfg.occluded_grace_ms, 1_000);
        assert_eq!(cfg.lock_grace_ms, 1_000);
    }
}
