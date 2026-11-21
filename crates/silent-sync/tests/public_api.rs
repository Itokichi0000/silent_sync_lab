//! Keeps the public API visible: every line that declares something public in
//! the two library crates is listed in `docs/api/`, and this test fails when
//! the source and the list differ. Changing the API therefore always shows up
//! as a change to those files.
//!
//! The list is textual (declaration lines, in source order), not a semantic
//! model of the API: it cannot judge compatibility, it only makes sure no
//! public item appears, disappears or changes its signature line unnoticed.
//!
//! To accept a change, regenerate the lists:
//!
//! ```sh
//! UPDATE_API_LISTS=1 cargo test -p silent-sync --test public_api
//! ```
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The declaration up to (not including) its body or terminator.
fn declaration(text: &str) -> String {
    let cut = text
        .find(" {")
        .or_else(|| text.find(';'))
        .unwrap_or(text.len());
    text[..cut].trim_end_matches(['{', ' ']).to_string()
}

/// Public declarations of one source file, in source order:
///
/// * every line that starts with `pub ` (items, fields, methods), with a
///   signature that spans several lines joined into one;
/// * the variants of public enums;
/// * methods declared through `low_level!` (public with that feature);
/// * each with the `cfg` attribute directly above it, if any.
///
/// Fields of items that are not themselves public are skipped, and test
/// modules are not part of the API.
fn public_lines(source: &str) -> Vec<String> {
    #[derive(PartialEq)]
    enum Inside {
        Nothing,
        PublicEnum,
        PrivateItem,
    }
    let mut found = Vec::new();
    let mut previous = String::new();
    let mut inside = Inside::Nothing;
    let mut in_low_level = false;
    let mut lines = source.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = raw.trim();
        let top_level = !raw.starts_with(' ');
        if line == "#[cfg(test)]" {
            break;
        }
        let cfg = if previous.starts_with("#[cfg(") {
            format!("{previous} ")
        } else {
            String::new()
        };
        // A signature that continues on the following lines.
        let mut joined = line.to_string();
        let is_signature = line.starts_with("pub ") || (in_low_level && line.starts_with("fn "));
        if is_signature && line.ends_with('(') {
            for next in lines.by_ref() {
                let next = next.trim();
                joined.push_str(next);
                if next.ends_with('{') || next.ends_with(';') {
                    break;
                }
                joined.push(' ');
            }
        }

        if top_level {
            if line.starts_with("pub enum ") {
                inside = Inside::PublicEnum;
            } else if line.starts_with("pub(crate) struct ")
                || line.starts_with("struct ")
                || line.starts_with("pub(crate) enum ")
                || line.starts_with("enum ")
            {
                inside = Inside::PrivateItem;
            } else if line == "}" || line.starts_with("pub ") || line.starts_with("impl") {
                inside = Inside::Nothing;
            }
        }

        if line == "low_level! {" {
            in_low_level = true;
        } else if in_low_level && line.starts_with("fn ") {
            // Public with the `low-level` feature, crate-internal without it.
            found.push(format!(
                "{cfg}#[cfg(feature = \"low-level\")] pub {}",
                declaration(&joined)
            ));
            in_low_level = false;
        } else if line.starts_with("pub ") && (top_level || inside != Inside::PrivateItem) {
            found.push(format!("{cfg}{}", declaration(&joined)));
        } else if inside == Inside::PublicEnum
            && raw.starts_with("    ")
            && !raw.starts_with("     ")
            && line.chars().next().is_some_and(char::is_uppercase)
        {
            found.push(format!(
                "{cfg}  variant {}",
                declaration(line).trim_end_matches(',')
            ));
        }
        if !line.starts_with("///") && !line.is_empty() {
            previous = line.to_string();
        }
    }
    found
}

fn listing(crate_dir: &Path) -> String {
    let src = crate_dir.join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut out = String::new();
    for file in files {
        let lines = public_lines(&std::fs::read_to_string(&file).unwrap());
        if lines.is_empty() {
            continue;
        }
        let name = file.strip_prefix(&src).unwrap().display().to_string();
        let _ = writeln!(out, "## {}", name.replace('\\', "/"));
        for line in lines {
            let _ = writeln!(out, "{line}");
        }
        out.push('\n');
    }
    out
}

#[test]
fn the_public_api_matches_the_committed_lists() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let lists = crates.parent().unwrap().join("docs/api");
    let update = std::env::var_os("UPDATE_API_LISTS").is_some();
    for name in ["silent-sync", "silent-sync-sqlite"] {
        let actual = listing(&crates.join(name));
        assert!(actual.contains("pub "), "{name}: nothing found");
        let path = lists.join(format!("{name}.txt"));
        if update {
            std::fs::write(&path, &actual).unwrap();
            continue;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        if committed != actual {
            let old: Vec<&str> = committed.lines().collect();
            let new: Vec<&str> = actual.lines().collect();
            let removed: Vec<&&str> = old.iter().filter(|l| !new.contains(l)).collect();
            let added: Vec<&&str> = new.iter().filter(|l| !old.contains(l)).collect();
            panic!(
                "the public API of `{name}` differs from docs/api/{name}.txt\n\
                 no longer present: {removed:#?}\nnew: {added:#?}\n\
                 If the change is intended, run\n  \
                 UPDATE_API_LISTS=1 cargo test -p silent-sync --test public_api\n\
                 and commit the updated list."
            );
        }
    }
}

#[test]
fn declaration_lines_are_extracted_as_documented() {
    let source = "\
/// Doc.
pub struct A {
    pub field: u32,
    private: u32,
}
pub(crate) struct Hidden {
    pub field_of_a_private_item: u32,
}
#[cfg(feature = \"x\")]
/// Doc between the attribute and the item.
pub fn gated(
    first: u32,
    second: &str,
) -> u32 {
    first
}
#[non_exhaustive]
pub enum E {
    /// Doc.
    Unit,
    Tuple(u32),
    Struct {
        inner: u32,
    },
}
pub(crate) fn internal() {}
impl A {
    low_level! {
    /// Doc.
    fn building_block(&self) -> u32 {
        1
    }
    }
    pub const fn value(&self) -> u32;
}
#[cfg(test)]
mod tests {
    pub fn not_api() {}
}
";
    assert_eq!(
        public_lines(source),
        vec![
            "pub struct A",
            "pub field: u32,",
            "#[cfg(feature = \"x\")] pub fn gated(first: u32, second: &str, ) -> u32",
            "pub enum E",
            "  variant Unit",
            "  variant Tuple(u32)",
            "  variant Struct",
            "#[cfg(feature = \"low-level\")] pub fn building_block(&self) -> u32",
            "pub const fn value(&self) -> u32",
        ]
    );
}
