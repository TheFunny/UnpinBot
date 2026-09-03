//! Environment-driven configuration.

use std::path::PathBuf;

/// Runtime configuration, sourced entirely from environment variables.
#[derive(Clone, Debug)]
pub struct Config {
    /// Bot token from @BotFather, read from `TELOXIDE_TOKEN` (required).
    pub token: String,
    /// UI language: `"en"` or `"zh"`, read from `UNPINBOT_LANG` (default `"en"`).
    pub lang: String,
    /// Enabled-state file, read from `UNPINBOT_STATE_PATH`
    /// (default `pers_data/state.json`).
    pub state_path: PathBuf,
}

const DEFAULT_STATE_PATH: &str = "pers_data/state.json";

fn non_empty(var: &str) -> Result<String, String> {
    match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        Ok(_) => Err(format!("environment variable {var} is set but empty")),
        Err(_) => Err(format!(
            "environment variable {var} is not set (get a token from @BotFather)"
        )),
    }
}

fn defaulted(var: &str, default: &str) -> String {
    match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

impl Config {
    /// Reads configuration from the environment. Fails with a message naming
    /// the offending variable; never panics.
    pub fn from_env() -> Result<Config, String> {
        Ok(Config {
            token: non_empty("TELOXIDE_TOKEN")?,
            lang: defaulted("UNPINBOT_LANG", "en"),
            state_path: PathBuf::from(defaulted("UNPINBOT_STATE_PATH", DEFAULT_STATE_PATH)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var mutation is process-global; serialize the tests that touch it.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_env(f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        f();
    }

    #[test]
    fn missing_token_is_error() {
        with_env(|| {
            std::env::remove_var("TELOXIDE_TOKEN");
            let err = Config::from_env().unwrap_err();
            assert!(err.contains("TELOXIDE_TOKEN"));
        });
    }

    #[test]
    fn empty_token_is_rejected() {
        with_env(|| {
            std::env::set_var("TELOXIDE_TOKEN", "   ");
            let err = Config::from_env().unwrap_err();
            assert!(err.contains("TELOXIDE_TOKEN"));
        });
    }

    #[test]
    fn defaults_apply_when_unset() {
        with_env(|| {
            std::env::set_var("TELOXIDE_TOKEN", "123:abc");
            std::env::remove_var("UNPINBOT_LANG");
            std::env::remove_var("UNPINBOT_STATE_PATH");
            let cfg = Config::from_env().expect("valid config");
            assert_eq!(cfg.lang, "en");
            assert_eq!(cfg.state_path, PathBuf::from("pers_data/state.json"));
        });
    }

    #[test]
    fn overrides_apply_when_set() {
        with_env(|| {
            std::env::set_var("TELOXIDE_TOKEN", "123:abc");
            std::env::set_var("UNPINBOT_LANG", "zh");
            std::env::set_var("UNPINBOT_STATE_PATH", "custom/state.json");
            let cfg = Config::from_env().expect("valid config");
            assert_eq!(cfg.lang, "zh");
            assert_eq!(cfg.state_path, PathBuf::from("custom/state.json"));
        });
    }
}
