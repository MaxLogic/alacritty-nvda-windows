//! Accessibility support.

#[cfg(windows)]
pub mod event_throttle;
#[cfg(windows)]
pub mod range_from_point;
pub mod snapshot;
#[cfg(windows)]
pub mod text_pattern;
#[cfg(windows)]
pub mod text_range;
#[cfg(windows)]
pub mod windows_provider;
