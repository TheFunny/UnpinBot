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

/// Languages with embedded catalogs, lowercase ISO 639-1 codes.
pub const SUPPORTED: [&str; 2] = ["en", "zh"];

/// Language used when the sender's language is unknown or unsupported.
pub const FALLBACK: &str = "en";

/// Both embedded catalogs, loaded once at startup.
pub struct Catalogs {
    en: Lang,
    zh: Lang,
}

impl Catalogs {
    /// Parses both embedded catalogs. A parse failure is a startup error.
    pub fn load() -> Result<Self, String> {
        let en: Lang = serde_json::from_str(EN)
            .map_err(|e| format!("built-in en language file is invalid: {e}"))?;
        let zh: Lang = serde_json::from_str(ZH)
            .map_err(|e| format!("built-in zh language file is invalid: {e}"))?;
        Ok(Self { en, zh })
    }

    /// Picks the catalog for an IETF language tag: the primary subtag
    /// (`zh-Hans-CN` -> `zh`) is matched against [`SUPPORTED`]; anything
    /// else falls back to [`FALLBACK`].
    pub fn resolve(&self, code: Option<&str>) -> &Lang {
        let Some(code) = code else { return &self.en };
        let primary = code.split('-').next().unwrap_or(FALLBACK).to_lowercase();
        match primary.as_str() {
            "zh" => &self.zh,
            _ => &self.en,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_catalog_is_complete() {
        let catalogs = Catalogs::load().expect("catalogs parse");
        let lang = &catalogs.en;
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
        let catalogs = Catalogs::load().expect("catalogs parse");
        let lang = &catalogs.zh;
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
    fn resolve_matches_primary_subtag_with_fallback() {
        let catalogs = Catalogs::load().expect("catalogs parse");
        assert_eq!(catalogs.resolve(None).start, catalogs.en.start);
        assert_eq!(
            catalogs.resolve(Some("zh-Hans-CN")).start,
            catalogs.zh.start
        );
        assert_eq!(catalogs.resolve(Some("en-US")).start, catalogs.en.start);
        assert_eq!(catalogs.resolve(Some("pt-BR")).start, catalogs.en.start);
    }
}
