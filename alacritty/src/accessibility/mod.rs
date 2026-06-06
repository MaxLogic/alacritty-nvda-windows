//! Accessibility support.

pub mod snapshot;
#[cfg(windows)]
pub mod text_pattern;
#[cfg(windows)]
pub mod text_range;
#[cfg(windows)]
pub mod windows_provider;
