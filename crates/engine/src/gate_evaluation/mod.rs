//! Versioned external evaluator contract. Stage disposition and operator
//! consent remain engine-owned; a subprocess supplies only a check result.
pub mod protocol;

pub mod artifacts;
pub(crate) mod driver;
pub mod evidence;
pub mod input_builder;
pub mod lifecycle;
pub mod snapshot;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod subprocess;

pub(crate) mod authority;
