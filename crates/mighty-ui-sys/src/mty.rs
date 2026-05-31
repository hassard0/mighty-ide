//! Shared `mty` compiler discovery for all shim-owned features.

use std::path::Path;

/// Resolve the `mty` compiler path.
///
/// Order:
/// 1. `MIGHTY_MTY` explicit override.
/// 2. Known local stardust release/debug builds.
/// 3. Bare `mty`, relying on `PATH`.
pub fn path() -> String {
    if let Ok(p) = std::env::var("MIGHTY_MTY") {
        if !p.trim().is_empty() {
            return p;
        }
    }
    for candidate in [
        r"C:\Users\ihass\stardust\target\release\mty.exe",
        r"C:\Users\ihass\stardust\target\debug\mty.exe",
        r"C:\Users\ihass\stardust-v035-T2\target\debug\mty.exe",
    ] {
        if Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    "mty".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins() {
        let old = std::env::var_os("MIGHTY_MTY");
        std::env::set_var("MIGHTY_MTY", "C:/custom/mty.exe");
        assert_eq!(path(), "C:/custom/mty.exe");
        if let Some(v) = old {
            std::env::set_var("MIGHTY_MTY", v);
        } else {
            std::env::remove_var("MIGHTY_MTY");
        }
    }
}
