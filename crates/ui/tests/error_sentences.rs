//! Every sentence the fetch crate can show is translated in every language.
#![cfg(feature = "network")]

use jizzcraft_ui::lang::{de, es, fr, it, pt};

#[test]
fn every_error_sentence_is_translated() {
    let tables: [(&str, &[(&str, &str)]); 5] = [
        ("fr", fr::TABLE),
        ("es", es::TABLE),
        ("de", de::TABLE),
        ("it", it::TABLE),
        ("pt", pt::TABLE),
    ];
    let mut missing = Vec::new();
    for (code, t) in tables {
        for s in jizzcraft_fetch::errors::ALL {
            if !t.iter().any(|(en, _)| en == s) {
                missing.push(format!("{code}: {s}"));
            }
        }
    }
    assert!(missing.is_empty(), "untranslated:\n{}", missing.join("\n"));
}
