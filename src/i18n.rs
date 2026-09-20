//! Compile-time embedded UI strings (`lang/{en,zh}.json`).

use std::sync::Arc;

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
    pub rights_revoked: String,
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

/// Every embedded catalog, keyed by lowercase ISO 639-1 primary subtag. This
/// table is the only place a language is declared: [`Catalogs::load`] parses
/// it and [`Catalogs::resolve`] matches against it.
const EMBEDDED: [(&str, &str); 2] = [("en", EN), ("zh", ZH)];

/// Language used when the sender's language is unknown or unsupported.
pub const FALLBACK: &str = "en";

/// All embedded catalogs, parsed once at startup.
pub struct Catalogs {
    /// `Arc`, so [`Catalogs::resolve`] can hand a catalog to a handler with a
    /// refcount bump instead of cloning every string in it per update.
    langs: Vec<(&'static str, Arc<Lang>)>,
}

impl Catalogs {
    /// Parses every embedded catalog. A parse failure is a startup error.
    pub fn load() -> Result<Self, String> {
        let langs = EMBEDDED
            .iter()
            .map(|(code, json)| {
                serde_json::from_str(json)
                    .map(|lang| (*code, Arc::new(lang)))
                    .map_err(|e| format!("built-in {code} language file is invalid: {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert!(
            langs.iter().any(|(code, _)| *code == FALLBACK),
            "built-in {FALLBACK} catalog is missing"
        );
        Ok(Self { langs })
    }

    /// Every embedded catalog with its language code, in declaration order.
    pub fn all(&self) -> impl Iterator<Item = (&'static str, Arc<Lang>)> + '_ {
        self.langs
            .iter()
            .map(|(code, lang)| (*code, Arc::clone(lang)))
    }

    /// Picks the catalog for an IETF language tag: the primary subtag
    /// (`zh-Hans-CN` -> `zh`) is matched against the embedded catalogs;
    /// anything else falls back to [`FALLBACK`].
    pub fn resolve(&self, code: Option<&str>) -> Arc<Lang> {
        let primary = code
            .and_then(|code| code.split('-').next())
            .map(str::to_lowercase);
        self.langs
            .iter()
            .find(|(code, _)| Some(*code) == primary.as_deref())
            .or_else(|| self.langs.iter().find(|(code, _)| *code == FALLBACK))
            .map(|(_, lang)| Arc::clone(lang))
            .expect("load() guarantees a fallback catalog")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalogs_are_complete() {
        let catalogs = Catalogs::load().expect("catalogs parse");
        let codes: Vec<_> = catalogs.all().map(|(code, _)| code).collect();
        assert_eq!(codes, ["en", "zh"], "embedded language list changed");
        for (code, lang) in catalogs.all() {
            for (field, s) in [
                ("start", &lang.start),
                ("help", &lang.help),
                ("enable", &lang.enable),
                ("disable", &lang.disable),
                ("description", &lang.description),
                ("error.not_group", &lang.error.not_group),
                ("error.not_admin", &lang.error.not_admin),
                ("error.require_rights", &lang.error.require_rights),
                ("error.rights_revoked", &lang.error.rights_revoked),
                ("error.already_enabled", &lang.error.already_enabled),
                ("error.already_disabled", &lang.error.already_disabled),
                ("error.retry_later", &lang.error.retry_later),
                ("cmd.start", &lang.cmd.start),
                ("cmd.help", &lang.cmd.help),
                ("cmd.enable", &lang.cmd.enable),
                ("cmd.disable", &lang.cmd.disable),
            ] {
                assert!(!s.is_empty(), "{code}: {field} is empty");
            }
        }
    }

    #[test]
    fn resolve_matches_primary_subtag_with_fallback() {
        let catalogs = Catalogs::load().expect("catalogs parse");
        let en = catalogs.resolve(Some("en")).start.clone();
        let zh = catalogs.resolve(Some("zh")).start.clone();
        assert_ne!(en, zh, "catalogs are not distinct");

        assert_eq!(catalogs.resolve(None).start, en);
        assert_eq!(catalogs.resolve(Some("zh-Hans-CN")).start, zh);
        assert_eq!(catalogs.resolve(Some("ZH")).start, zh);
        assert_eq!(catalogs.resolve(Some("en-US")).start, en);
        assert_eq!(catalogs.resolve(Some("pt-BR")).start, en);
    }
}
