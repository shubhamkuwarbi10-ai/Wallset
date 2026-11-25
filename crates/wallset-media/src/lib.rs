//! Media layer for wallSet: a dynamically loaded libmpv binding, file probing,
//! and the single render profile shared by preview and wallpaper.

pub mod mpv;
pub mod probe;
pub mod render;

pub use mpv::{Controller, Event, Library, Mpv, MpvError};
pub use probe::{ProbeError, probe};
pub use render::{FitMode, RenderProfile};
