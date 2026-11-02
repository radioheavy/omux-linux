//! Turkish translation of the interface. Strings are written in English in
//! the code and looked up here; anything missing stays English. The
//! language follows the system (`LC_ALL`, `LC_MESSAGES`, `LANG`) unless
//! Settings › Language picks one.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static TURKISH: AtomicBool = AtomicBool::new(false);

/// Whether a locale name such as `tr_TR.UTF-8` is Turkish.
fn is_turkish(locale: &str) -> bool {
    locale.starts_with("tr")
}

/// The system's language: the first of `LC_ALL`, `LC_MESSAGES`, `LANG` set.
fn system_is_turkish() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .is_some_and(|v| is_turkish(&v))
}

/// Picks the language: "tr", "en", or "auto" for the system's.
pub fn set_language(choice: &str) {
    let turkish = match choice {
        "tr" => true,
        "en" => false,
        _ => system_is_turkish(),
    };
    TURKISH.store(turkish, Ordering::Relaxed);
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| crate::i18n_tr::STRINGS.iter().copied().collect())
}

/// The interface text for `en` in the current language. A key may carry a
/// context before `|` ("pr|Open") when one English word needs two
/// translations; English shows the part after it.
pub fn tr(en: &str) -> &str {
    let found = TURKISH
        .load(Ordering::Relaxed)
        .then(|| table().get(en).copied())
        .flatten();
    found.unwrap_or_else(|| en.split_once('|').map_or(en, |(_, text)| text))
}

/// `tr`, then fills `{name}` placeholders: `trf!("Added {n}", n = 3)`.
#[macro_export]
macro_rules! trf {
    ($en:literal $(, $key:ident = $value:expr)* $(,)?) => {{
        #[allow(unused_mut)]
        let mut text = $crate::i18n::tr($en).to_string();
        $(
            text = text.replace(concat!("{", stringify!($key), "}"), &($value).to_string());
        )*
        text
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(s: &str) -> Vec<String> {
        let mut found: Vec<String> = s
            .split('{')
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(k, _)| k.to_string()))
            .collect();
        found.sort();
        found
    }

    #[test]
    fn translations_keep_placeholders() {
        let mut seen = std::collections::HashSet::new();
        for (en, tr) in crate::i18n_tr::STRINGS {
            assert!(seen.insert(*en), "translated twice: {en}");
            assert!(!tr.is_empty(), "empty translation: {en}");
            assert_eq!(
                placeholders(en),
                placeholders(tr),
                "placeholders differ: {en}"
            );
        }
    }

    #[test]
    fn context_keys_show_their_text() {
        set_language("en");
        assert_eq!(tr("pr|Open"), "Open");
        assert_eq!(tr("Open"), "Open");
    }

    #[test]
    fn locales() {
        assert!(is_turkish("tr_TR.UTF-8"));
        assert!(!is_turkish("en_US.UTF-8"));
        assert!(!is_turkish("C"));
    }
}
