//! The language of the ICE's text (stop reasons, OS call descriptions,
//! explanations): Japanese or English, one setting for the process.
//! The library default is Japanese (tests and snapshots depend on it);
//! `rs6809 --lang` and the UI's switch change it. Text already recorded
//! (call arguments and results) stays in the language it was made in.
use std::sync::atomic::{AtomicBool, Ordering};

static EN: AtomicBool = AtomicBool::new(false);

pub fn en() -> bool {
    EN.load(Ordering::Relaxed)
}
pub fn set_en(on: bool) {
    EN.store(on, Ordering::Relaxed)
}
/// "ja" or "en".
pub fn code() -> &'static str {
    if en() { "en" } else { "ja" }
}
/// English unless the environment asks for Japanese (LC_ALL, LC_MESSAGES, LANG).
pub fn english_from_env() -> bool {
    let v = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    !v.starts_with("ja")
}

/// `tr!(japanese, english)`: the one for the current language (only that
/// argument is evaluated).
#[macro_export]
macro_rules! tr {
    ($ja:expr, $en:expr $(,)?) => {
        if $crate::ice::lang::en() { $en } else { $ja }
    };
}
