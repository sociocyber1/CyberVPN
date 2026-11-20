//! Russian / English. The language follows the system (LANG, LC_*, or the
//! Windows UI language) unless chosen in Settings.

use crate::store::Language;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static RU: AtomicBool = AtomicBool::new(false);

/// Detect the system language: true = Russian.
pub fn system_is_ru() -> bool {
    #[cfg(windows)]
    {
        // LANGID primary language 0x19 = Russian
        extern "system" {
            fn GetUserDefaultUILanguage() -> u16;
        }
        let id = unsafe { GetUserDefaultUILanguage() };
        if id & 0x3ff == 0x19 {
            return true;
        }
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim();
            if v.is_empty() || v == "C" || v == "POSIX" {
                continue;
            }
            // LANGUAGE may be a list: "ru:en"
            return v.split(':').next().unwrap_or("").to_lowercase().starts_with("ru");
        }
    }
    false
}

pub fn set(lang: Language) {
    let ru = match lang {
        Language::Ru => true,
        Language::En => false,
        Language::Auto => system_is_ru(),
    };
    RU.store(ru, Ordering::Relaxed);
}

pub fn is_ru() -> bool {
    RU.load(Ordering::Relaxed)
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    T.get_or_init(|| RU_TABLE.iter().copied().collect())
}

/// Translate a UI string (English is the key).
pub fn tr(s: &'static str) -> &'static str {
    if is_ru() {
        table().get(s).copied().unwrap_or(s)
    } else {
        s
    }
}

/// Translate, then fill `{}` placeholders in order.
pub fn trf(s: &'static str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = String::new();
    let mut it = args.iter();
    let mut parts = tr(s).split("{}").peekable();
    while let Some(p) = parts.next() {
        out.push_str(p);
        if parts.peek().is_some() {
            if let Some(a) = it.next() {
                out.push_str(&a.to_string());
            }
        }
    }
    out
}

include!("i18n_ru.rs");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_and_formats() {
        RU.store(true, Ordering::Relaxed);
        assert_eq!(tr("Protected"), "Защищено");
        assert_eq!(trf("Connected to {}", &[&"NL"]), "Подключено к NL");
        assert_eq!(tr("some untranslated text"), "some untranslated text");
        RU.store(false, Ordering::Relaxed);
        assert_eq!(trf("Connected to {}", &[&"NL"]), "Connected to NL");
    }

    #[test]
    fn every_ru_entry_keeps_placeholders() {
        for (en, ru) in RU_TABLE {
            assert_eq!(en.matches("{}").count(), ru.matches("{}").count(), "placeholder mismatch: {en}");
        }
    }
}
