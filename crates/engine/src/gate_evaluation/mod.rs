//! Versioned external evaluator contract. Stage disposition and operator
//! consent remain engine-owned; a subprocess supplies only a check result.
pub mod protocol;

pub mod artifacts;
pub mod evidence;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod subprocess;
