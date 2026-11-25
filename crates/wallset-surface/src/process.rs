//! How wallSet's own processes behave towards the rest of the system, and
//! how their cost is measured.
//!
//! ## Measuring honestly
//!
//! [`ProcessStats`] reports **both** working set and private bytes. Working
//! set is what Task Manager shows by default and is easy to make look small:
//! trimming it just moves pages to the standby list, from which they fault
//! straight back in when touched. Private bytes is the memory the process has
//! actually committed and cannot be gamed that way. Any benchmark quoting only
//! working set is quoting the flattering number.

/// Resource usage snapshot for one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcessStats {
    /// Physical memory currently mapped (Task Manager's default "Memory").
    pub working_set_bytes: u64,
    /// Committed private memory. The number that cannot be gamed.
    pub private_bytes: u64,
    /// Total CPU time consumed so far, user + kernel, in 100 ns units.
    pub cpu_time_100ns: u64,
}

impl ProcessStats {
    /// CPU usage between two snapshots, as a percentage of **one** core.
    #[must_use]
    pub fn cpu_percent_since(&self, earlier: &Self, wall: std::time::Duration) -> f64 {
        let wall_100ns = wall.as_nanos() as f64 / 100.0;
        if wall_100ns <= 0.0 {
            return 0.0;
        }
        self.cpu_time_100ns.saturating_sub(earlier.cpu_time_100ns) as f64 / wall_100ns * 100.0
    }
}

#[cfg(windows)]
mod imp {
    use super::ProcessStats;
    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{
        BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, GetProcessTimes, MEMORY_PRIORITY_INFORMATION,
        MEMORY_PRIORITY_LOW, OpenProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
        PROCESS_QUERY_LIMITED_INFORMATION, ProcessMemoryPriority, ProcessPowerThrottling,
        SetPriorityClass, SetProcessInformation,
    };

    /// Declare per-monitor-v2 DPI awareness. **Must run before any window or
    /// display API call.** Without it Windows virtualises coordinates for this
    /// process: on a 125%-scaled 1920x1080 panel every API reports 1536x864,
    /// and a wallpaper window would cover only the top-left 80% of the screen.
    pub fn init_dpi_awareness() {
        use windows::Win32::UI::HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
        };
        // SAFETY: process-wide setting; fails harmlessly if already set (for
        // example by an embedded manifest).
        let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    }

    /// Mark the current process as background work:
    ///
    /// * **EcoQoS** (Windows 11 "Efficiency mode"): the scheduler prefers
    ///   efficiency cores and lower clock speeds for this process.
    /// * **Low memory priority**: under memory pressure, our pages are
    ///   trimmed before the foreground application's.
    /// * **Below-normal CPU priority**: we always yield to what the user is
    ///   actually doing.
    ///
    /// Each step is best-effort; older Windows versions ignore EcoQoS.
    pub fn enter_background_mode() {
        // SAFETY: pseudo-handle for the current process, never closed.
        let me = unsafe { GetCurrentProcess() };

        let throttle = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        };
        // SAFETY: struct and size match the information class.
        let _ = unsafe {
            SetProcessInformation(
                me,
                ProcessPowerThrottling,
                (&raw const throttle).cast(),
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        };

        let mem = MEMORY_PRIORITY_INFORMATION { MemoryPriority: MEMORY_PRIORITY_LOW };
        // SAFETY: as above.
        let _ = unsafe {
            SetProcessInformation(
                me,
                ProcessMemoryPriority,
                (&raw const mem).cast(),
                size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
            )
        };

        // SAFETY: valid pseudo-handle.
        let _ = unsafe { SetPriorityClass(me, BELOW_NORMAL_PRIORITY_CLASS) };
    }

    pub fn stats(pid: u32) -> Option<ProcessStats> {
        // SAFETY: limited-information access is granted for same-user processes.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let r = stats_for(h);
        // SAFETY: we opened it.
        unsafe {
            let _ = CloseHandle(h);
        }
        r
    }

    pub fn current_stats() -> Option<ProcessStats> {
        // SAFETY: pseudo-handle, must not be closed.
        stats_for(unsafe { GetCurrentProcess() })
    }

    fn stats_for(h: HANDLE) -> Option<ProcessStats> {
        let mut pmc = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: the EX struct starts with the base struct and cb says so.
        unsafe {
            GetProcessMemoryInfo(h, (&raw mut pmc).cast::<PROCESS_MEMORY_COUNTERS>(), pmc.cb)
        }
        .ok()?;

        let (mut c, mut e, mut k, mut u) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: four valid out-params.
        unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) }.ok()?;
        let ft = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);

        Some(ProcessStats {
            working_set_bytes: pmc.WorkingSetSize as u64,
            private_bytes: pmc.PrivateUsage as u64,
            cpu_time_100ns: ft(k) + ft(u),
        })
    }
}

#[cfg(not(windows))]
mod imp {
    use super::ProcessStats;
    pub fn init_dpi_awareness() {}
    pub fn enter_background_mode() {
        // Linux: setpriority(PRIO_PROCESS, 0, 10) and SCHED_IDLE-adjacent
        // policies; macOS: setpriority + QoS class QOS_CLASS_BACKGROUND.
    }
    pub fn stats(_pid: u32) -> Option<ProcessStats> {
        None
    }
    pub fn current_stats() -> Option<ProcessStats> {
        None
    }
}

pub use imp::{current_stats, enter_background_mode, init_dpi_awareness, stats};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cpu_percent_is_relative_to_one_core() {
        let a = ProcessStats { cpu_time_100ns: 0, ..Default::default() };
        // 0.5 s of CPU over 1 s of wall time = 50% of one core.
        let b = ProcessStats { cpu_time_100ns: 5_000_000, ..Default::default() };
        assert!((b.cpu_percent_since(&a, Duration::from_secs(1)) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn counter_going_backwards_reads_as_zero_not_garbage() {
        let a = ProcessStats { cpu_time_100ns: 100, ..Default::default() };
        let b = ProcessStats { cpu_time_100ns: 50, ..Default::default() };
        assert_eq!(b.cpu_percent_since(&a, Duration::from_secs(1)), 0.0);
    }
}
