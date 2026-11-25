//! Live preview: the real `wallset-render`, embedded in a child window.
//!
//! Not an approximation. Same binary, same render profile, same decoder and
//! GPU choice as the wallpaper itself. Only the window is smaller, and since
//! cost is dominated by the *source* resolution and the GPU driver rather
//! than the output size (docs/BENCHMARKS.md), the CPU and memory measured
//! here are what the wallpaper will cost on this machine.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use wallset_core::{FitMode, RenderSettings};
use wallset_surface::HostWindow;
use wallset_surface::process::{self, ProcessStats};

/// Startup allocations settle within a few seconds; measuring from launch
/// would overstate steady-state cost.
const WARMUP: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy)]
pub struct Measured {
    pub cpu_percent: f64,
    pub private_mb: f64,
    pub working_set_mb: f64,
    pub over_secs: f64,
}

pub struct Preview {
    window: HostWindow,
    child: Child,
    started: Instant,
    baseline: Option<(ProcessStats, Instant)>,
    last_sample: Instant,
    pub measured: Option<Measured>,
    rect: (i32, i32, u32, u32),
}

impl Preview {
    pub fn start(
        parent: isize,
        rect: (i32, i32, u32, u32),
        file: &Path,
        fit: FitMode,
        render: &RenderSettings,
        adapter: Option<&str>,
    ) -> Result<Self, String> {
        let window = HostWindow::create_child(parent, rect.0, rect.1, rect.2, rect.3).map_err(|e| e.to_string())?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("wallset-render.exe");
        let mut cmd = Command::new(&exe);
        cmd.args(["--wid", &window.raw().to_string(), "--file"]).arg(file).args(["--fit", fit.as_arg()]);
        if let Some(a) = adapter {
            cmd.args(["--adapter", a]);
        }
        if !render.hardware_decode {
            cmd.arg("--no-hwdec");
        }
        if !render.low_power {
            cmd.arg("--quality");
        }
        if !render.efficiency_mode {
            cmd.arg("--foreground");
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
        Ok(Self {
            window,
            child,
            started: Instant::now(),
            baseline: None,
            last_sample: Instant::now(),
            measured: None,
            rect,
        })
    }

    /// Keep the child window over the reserved area. Cheap when unchanged.
    pub fn place(&mut self, rect: (i32, i32, u32, u32)) {
        if rect != self.rect {
            self.window.set_client_rect(rect.0, rect.1, rect.2, rect.3);
            self.rect = rect;
        }
    }

    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Take a measurement, at most once a second. CPU is computed over the
    /// whole post-warm-up window (precise), not per sample (quantised to
    /// ~1.56% by Windows' 15.6 ms CPU-time tick).
    pub fn sample(&mut self) {
        if self.started.elapsed() < WARMUP || self.last_sample.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_sample = Instant::now();
        let Some(now) = process::stats(self.child.id()) else { return };
        let Some((base, t0)) = self.baseline else {
            self.baseline = Some((now, Instant::now()));
            return;
        };
        let over = t0.elapsed();
        self.measured = Some(Measured {
            cpu_percent: now.cpu_percent_since(&base, over),
            private_mb: now.private_bytes as f64 / 1_048_576.0,
            working_set_mb: now.working_set_bytes as f64 / 1_048_576.0,
            over_secs: over.as_secs_f64(),
        });
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, "quit");
        }
        // Give it a moment to exit cleanly, then make sure.
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline && matches!(self.child.try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
