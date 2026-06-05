//! Shared `mty` compiler discovery for all shim-owned features.

/// Resolve the `mty` compiler path.
///
/// Order:
/// 1. `MIGHTY_MTY` explicit override.
/// 2. Bare `mty`, relying on `PATH`.
pub fn path() -> String {
    if let Ok(p) = std::env::var("MIGHTY_MTY") {
        if !p.trim().is_empty() {
            return p;
        }
    }
    "mty".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn env_override_wins() {
        let _guard = ENV_LOCK.lock().unwrap();
        let old = std::env::var_os("MIGHTY_MTY");
        std::env::set_var("MIGHTY_MTY", "C:/custom/mty.exe");
        assert_eq!(path(), "C:/custom/mty.exe");
        if let Some(v) = old {
            std::env::set_var("MIGHTY_MTY", v);
        } else {
            std::env::remove_var("MIGHTY_MTY");
        }
    }

    #[test]
    fn empty_env_uses_path_command() {
        let _guard = ENV_LOCK.lock().unwrap();
        let old = std::env::var_os("MIGHTY_MTY");
        std::env::set_var("MIGHTY_MTY", "   ");
        assert_eq!(path(), "mty");
        if let Some(v) = old {
            std::env::set_var("MIGHTY_MTY", v);
        } else {
            std::env::remove_var("MIGHTY_MTY");
        }
    }
}
