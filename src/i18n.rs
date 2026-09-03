//! Compile-time embedded UI strings (`lang/{en,zh}.json`).

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Lang {
    pub start: String,
    pub help: String,
    pub enable: String,
    pub disable: String,
    pub error: Errors,
    pub cmd: Commands,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Errors {
    pub not_group: String,
    pub not_admin: String,
    pub require_rights: String,
    pub already_enabled: String,
    pub already_disabled: String,
    pub retry_later: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Commands {
    pub start: String,
    pub help: String,
    pub enable: String,
    pub disable: String,
}

const EN: &str = include_str!("../lang/en.json");
const ZH: &str = include_str!("../lang/zh.json");

/// Loads the catalog for `lang`. Unknown languages are a startup error.
pub fn load(lang: &str) -> Result<Lang, String> {
    let raw = match lang {
        "en" => EN,
        "zh" => ZH,
        other => {
            return Err(format!(
                "unsupported UNPINBOT_LANG {other:?} (supported: en, zh)"
            ))
        }
    };
    serde_json::from_str(raw).map_err(|e| format!("built-in {lang} language file is invalid: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_catalog_is_complete() {
        let lang = load("en").expect("en parses");
        assert!(!lang.start.is_empty());
        assert!(!lang.help.is_empty());
        assert!(!lang.enable.is_empty());
        assert!(!lang.disable.is_empty());
        assert!(!lang.description.is_empty());
        for s in [
            &lang.error.not_group,
            &lang.error.not_admin,
            &lang.error.require_rights,
            &lang.error.already_enabled,
            &lang.error.already_disabled,
            &lang.error.retry_later,
            &lang.cmd.start,
            &lang.cmd.help,
            &lang.cmd.enable,
            &lang.cmd.disable,
        ] {
            assert!(!s.is_empty());
        }
    }

    #[test]
    fn chinese_catalog_is_complete() {
        let lang = load("zh").expect("zh parses");
        assert!(!lang.error.retry_later.is_empty());
        assert!(!lang.cmd.enable.is_empty());
    }

    #[test]
    fn unknown_language_is_rejected() {
        let err = load("fr").unwrap_err();
        assert!(err.contains("en, zh"));
    }
}
