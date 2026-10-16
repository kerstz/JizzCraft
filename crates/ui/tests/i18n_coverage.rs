//! Every literal passed to `tr("…")` / `trf("…")` in the UI sources has an
//! entry in each of the five catalogs. Sources are scanned as text (all
//! features, `#[cfg(test)]` tails excluded), so a string added behind a
//! feature gate is checked too.
use jizzcraft_ui::lang::{de, es, fr, it, pt};
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n != "lang") {
                rs_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Decode a Rust string literal body (escapes and `\`-newline continuations).
fn unescape(raw: &str) -> String {
    let mut out = String::new();
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\n') => {
                while it.peek().is_some_and(|c| c.is_whitespace()) {
                    it.next();
                }
            }
            Some('u') => {
                let mut hex = String::new();
                for c in it.by_ref() {
                    if c == '}' {
                        break;
                    }
                    if c != '{' {
                        hex.push(c);
                    }
                }
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

/// Literals that are the first argument of a `tr(` or `trf(` call.
fn literals(src: &str) -> Vec<String> {
    let src = match src.find("#[cfg(test)]") {
        Some(i) => &src[..i],
        None => src,
    };
    let b = src.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let call = if b[i..].starts_with(b"trf(") {
            4
        } else if b[i..].starts_with(b"tr(") {
            3
        } else {
            i += 1;
            continue;
        };
        // Whole identifier: not `str(`, `ctr(`, `tr_dyn(`.
        let prev = if i > 0 { b[i - 1] } else { b' ' };
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            i += call;
            continue;
        }
        let mut j = i + call;
        while j < b.len() && (b[j] as char).is_whitespace() {
            j += 1;
        }
        if j < b.len() && b[j] == b'"' {
            let start = j + 1;
            let mut k = start;
            while k < b.len() && b[k] != b'"' {
                k += if b[k] == b'\\' { 2 } else { 1 };
            }
            found.push(unescape(&src[start..k.min(b.len())]));
            i = k;
        } else {
            i = j;
        }
    }
    found
}

#[test]
fn every_tr_literal_is_in_every_catalog() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    assert!(files.len() > 5, "sources not found");
    let mut keys: Vec<String> = files
        .iter()
        // Windows checkouts may convert to CRLF; a `\` continuation then ends in
        // `\r\n`, which `unescape` does not know.
        .flat_map(|f| literals(&std::fs::read_to_string(f).unwrap().replace("\r\n", "\n")))
        .collect();
    keys.sort();
    keys.dedup();
    assert!(keys.len() > 100, "scanner found only {} keys", keys.len());

    let tables: [(&str, &[(&str, &str)]); 5] = [
        ("fr", fr::TABLE),
        ("es", es::TABLE),
        ("de", de::TABLE),
        ("it", it::TABLE),
        ("pt", pt::TABLE),
    ];
    let mut missing = Vec::new();
    for (code, t) in tables {
        for k in &keys {
            if !t.iter().any(|(en, _)| en == k) {
                missing.push(format!("{code}: {k}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "{} missing translations:\n{}",
        missing.len(),
        missing.join("\n")
    );
}

#[test]
fn scanner_handles_multiline_and_escapes() {
    let src = "x(crate::i18n::tr(\n \"A \\\"q\\\" b\"\n));\nlet s = trf(\"{} of {}\", &[]);\nstr(\"no\"); tr_dyn(\"no\");\n#[cfg(test)]\nmod t { fn f() { tr(\"hidden\"); } }";
    assert_eq!(literals(src), vec!["A \"q\" b", "{} of {}"]);
}
