use std::sync::atomic::{AtomicU8, Ordering};

const LANGUAGE_AUTO: u8 = 0;
const LANGUAGE_ZH_CN: u8 = 1;
const LANGUAGE_EN_US: u8 = 2;

static DISPLAY_LANGUAGE: AtomicU8 = AtomicU8::new(LANGUAGE_AUTO);

pub(crate) fn set_display_language(language: &str) {
    let value = match language.trim().to_ascii_lowercase().as_str() {
        "zh-cn" => LANGUAGE_ZH_CN,
        "en-us" => LANGUAGE_EN_US,
        _ => LANGUAGE_AUTO,
    };
    DISPLAY_LANGUAGE.store(value, Ordering::Relaxed);
}

pub(crate) fn is_simplified_chinese() -> bool {
    match DISPLAY_LANGUAGE.load(Ordering::Relaxed) {
        LANGUAGE_ZH_CN => true,
        LANGUAGE_EN_US => false,
        _ => system_locale_is_simplified_chinese(),
    }
}

pub(crate) fn text(chinese: &'static str, english: &'static str) -> &'static str {
    if is_simplified_chinese() {
        chinese
    } else {
        english
    }
}

pub(crate) fn display_language_tag() -> &'static str {
    if is_simplified_chinese() {
        "zh-CN"
    } else {
        "en-US"
    }
}

fn system_locale_is_simplified_chinese() -> bool {
    let Some(locale) = sys_locale::get_locale() else {
        return false;
    };
    locale_is_simplified_chinese(&locale)
}

fn locale_is_simplified_chinese(locale: &str) -> bool {
    let normalized = locale
        .trim()
        .replace('_', "-")
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    normalized == "zh"
        || normalized.starts_with("zh-cn")
        || normalized.starts_with("zh-sg")
        || normalized.starts_with("zh-hans")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_language_overrides_the_system_locale() {
        set_display_language("zh-CN");
        assert!(is_simplified_chinese());
        assert_eq!(display_language_tag(), "zh-CN");
        set_display_language("en-US");
        assert!(!is_simplified_chinese());
        assert_eq!(display_language_tag(), "en-US");
        set_display_language("");
    }

    #[test]
    fn recognizes_macos_simplified_chinese_locale_variants() {
        for locale in [
            "zh",
            "zh-CN",
            "zh_CN.UTF-8",
            "zh-SG",
            "zh-Hans",
            "zh-Hans-CN",
        ] {
            assert!(locale_is_simplified_chinese(locale), "{locale}");
        }
        for locale in ["en-US", "ja-JP", "zh-TW", "zh-Hant"] {
            assert!(!locale_is_simplified_chinese(locale), "{locale}");
        }
    }
}
