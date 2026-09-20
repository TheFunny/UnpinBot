//! Environment-driven configuration.

use std::path::PathBuf;

/// Runtime configuration, sourced entirely from environment variables.
#[derive(Clone, Debug)]
pub struct Config {
    /// Bot token from @BotFather, read from `TELOXIDE_TOKEN` (required).
    pub token: String,
    /// Enabled-state file, read from `UNPINBOT_STATE_PATH`
    /// (default `pers_data/state.json`).
    pub state_path: PathBuf,
    /// Proxy for every outgoing request, read from `TELOXIDE_PROXY`; `None`
    /// connects directly.
    pub proxy: Option<reqwest::Proxy>,
}

const DEFAULT_STATE_PATH: &str = "pers_data/state.json";

fn non_empty(var: &str) -> Result<String, String> {
    match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_owned()),
        Ok(_) => Err(format!("environment variable {var} is set but empty")),
        Err(_) => Err(format!(
            "environment variable {var} is not set (get a token from @BotFather)"
        )),
    }
}

fn defaulted(var: &str, default: &str) -> String {
    match std::env::var(var) {
        // Trimmed: a trailing space or CR (CRLF `.env`, `set VAR=x ` on
        // Windows) would otherwise reach Telegram as part of the token and
        // fail as an unreachable server.
        Ok(v) if !v.trim().is_empty() => v.trim().to_owned(),
        _ => default.to_string(),
    }
}

/// Parses `TELOXIDE_PROXY` when it is set to something non-empty.
fn proxy() -> Result<Option<reqwest::Proxy>, String> {
    let Ok(url) = std::env::var("TELOXIDE_PROXY") else {
        return Ok(None);
    };
    let url = url.trim();
    if url.is_empty() {
        return Ok(None);
    }
    reqwest::Proxy::all(url)
        .map(Some)
        .map_err(|e| format!("invalid TELOXIDE_PROXY {url:?}: {e}"))
}

impl Config {
    /// Reads configuration from the environment. Fails with a message naming
    /// the offending variable; never panics.
    pub fn from_env() -> Result<Config, String> {
        Ok(Config {
            token: non_empty("TELOXIDE_TOKEN")?,
            state_path: PathBuf::from(defaulted("UNPINBOT_STATE_PATH", DEFAULT_STATE_PATH)),
            proxy: proxy()?,
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
            std::env::remove_var("UNPINBOT_STATE_PATH");
            let cfg = Config::from_env().expect("valid config");
            assert_eq!(cfg.state_path, PathBuf::from("pers_data/state.json"));
        });
    }

    #[test]
    fn overrides_apply_when_set() {
        with_env(|| {
            std::env::set_var("TELOXIDE_TOKEN", "123:abc");
            std::env::set_var("UNPINBOT_STATE_PATH", "custom/state.json");
            let cfg = Config::from_env().expect("valid config");
            assert_eq!(cfg.state_path, PathBuf::from("custom/state.json"));
        });
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        // A CRLF `.env` or `set VAR=x ` on Windows otherwise hands Telegram a
        // token with a trailing space, reported as an unreachable server.
        with_env(|| {
            std::env::set_var("TELOXIDE_TOKEN", " 123:abc\r\n");
            std::env::set_var("UNPINBOT_STATE_PATH", " custom/state.json ");
            let cfg = Config::from_env().expect("valid config");
            assert_eq!(cfg.token, "123:abc");
            assert_eq!(cfg.state_path, PathBuf::from("custom/state.json"));
        });
    }

    #[test]
    fn proxy_is_optional_and_validated_at_startup() {
        with_env(|| {
            std::env::set_var("TELOXIDE_TOKEN", "123:abc");

            std::env::remove_var("TELOXIDE_PROXY");
            assert!(Config::from_env().expect("valid config").proxy.is_none());

            std::env::set_var("TELOXIDE_PROXY", " socks5://127.0.0.1:1080 ");
            assert!(Config::from_env().expect("valid config").proxy.is_some());

            std::env::set_var("TELOXIDE_PROXY", "not a url");
            let err = Config::from_env().unwrap_err();
            assert!(err.contains("TELOXIDE_PROXY"), "{err}");
            std::env::remove_var("TELOXIDE_PROXY");
        });
    }
}
