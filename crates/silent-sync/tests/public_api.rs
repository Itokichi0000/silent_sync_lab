//! Keeps the public API visible: an outline of everything the two library
//! crates declare publicly is kept in `docs/api/`, and this test fails when
//! the source and the outline differ. A change to the API therefore shows up
//! as a change to those files, to be reviewed like any other.
//!
//! The outline is derived from the (rustfmt-formatted) source text, not from
//! the compiler. It records, per source file of a public module:
//!
//! * public structs, enums and traits **in full** (fields with their types,
//!   variants with their payloads, trait methods) with their attributes
//!   (`cfg`, `derive`, `non_exhaustive`, ...);
//! * public functions, constants, type aliases, modules and re-exports with
//!   their complete signature, `where` clauses included;
//! * every `impl` header, and the public methods of inherent `impl` blocks;
//! * macro definitions and macro invocations inside `impl` blocks, so that
//!   API produced by a macro cannot appear unnoticed;
//! * the `[features]` table of the manifest.
//!
//! What it cannot do: judge compatibility, see through macros, or know
//! whether an item in a private inline module is re-exported. It is a
//! tripwire, not a semantic model of the API.
//!
//! To accept a change, regenerate the outlines and commit them:
//!
//! ```sh
//! UPDATE_API_LISTS=1 cargo test -p silent-sync --test public_api
//! ```
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::{
    collections::BTreeSet,
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

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn is_comment_or_blank(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t.starts_with("//")
}

/// Code lines of `source` (comments and blank lines removed), each with its
/// indentation, dedented by `base`.
fn code_lines(source: &str) -> Vec<(usize, String)> {
    source
        .lines()
        .filter(|l| !is_comment_or_blank(l))
        .map(|l| (indent_of(l), l.trim().to_string()))
        .collect()
}

/// Joins a declaration that may span several lines, starting at `start`, up
/// to and including the line that ends it (`{`, `;` or `}`); returns the
/// joined text without a trailing ` {` and the index after it.
fn joined(lines: &[(usize, String)], start: usize) -> (String, usize) {
    let mut text = String::new();
    let mut i = start;
    while i < lines.len() {
        let line = &lines[i].1;
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line);
        i += 1;
        if line.ends_with('{') || line.ends_with(';') || line.ends_with('}') {
            break;
        }
    }
    let text = text.strip_suffix(" {}").unwrap_or(&text);
    (text.trim_end_matches('{').trim_end().to_string(), i)
}

/// Joins a statement (`use`, `const`, `type`, ...) up to its semicolon.
fn joined_statement(lines: &[(usize, String)], start: usize) -> String {
    let mut text = String::new();
    for (_, line) in &lines[start..] {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line);
        if line.ends_with(';') {
            break;
        }
    }
    text
}

/// Index of the first line after the item whose first line is `start` and
/// which sits at indentation `indent`: the line after its closing brace at
/// that indentation, or after its own last line when it has no block.
fn after_item(lines: &[(usize, String)], start: usize, indent: usize) -> usize {
    let (_, mut i) = joined(lines, start);
    let opened = lines[i - 1].1.ends_with('{');
    if !opened {
        return i;
    }
    while i < lines.len() {
        let (ind, text) = &lines[i];
        i += 1;
        if *ind == indent && (text == "}" || text.starts_with("}")) {
            break;
        }
    }
    i
}

/// Outline of the items found at indentation `indent` in `lines[from..to]`.
fn outline(
    lines: &[(usize, String)],
    from: usize,
    to: usize,
    indent: usize,
    prefix: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    let mut i = from;
    while i < to {
        let (ind, text) = &lines[i];
        if *ind != indent {
            i += 1;
            continue;
        }
        if text.starts_with("#[") || text.starts_with("#![") {
            attrs.push(text.clone());
            i += 1;
            continue;
        }
        let end = after_item(lines, i, indent).min(to);
        let taken = std::mem::take(&mut attrs);
        let with_attrs = |decl: &str| {
            if taken.is_empty() {
                format!("{prefix}{decl}")
            } else {
                format!("{prefix}{} {decl}", taken.join(" "))
            }
        };
        if taken.iter().any(|a| a == "#[cfg(test)]") {
            // Test-only items are not API.
        } else if text.starts_with("pub struct ")
            || text.starts_with("pub enum ")
            || text.starts_with("pub trait ")
            || text.starts_with("pub union ")
        {
            let (header, body_from) = joined(lines, i);
            out.push(with_attrs(&header));
            let is_struct = text.starts_with("pub struct ");
            let is_trait = text.starts_with("pub trait ");
            let mut j = body_from;
            while j + 1 < end {
                let (member_indent, member) = &lines[j];
                if is_trait && *member_indent == indent + 4 && member.ends_with('{') {
                    // A provided method: its signature, not its body.
                    let (signature, next) = joined(lines, j);
                    out.push(format!("{prefix}    {signature}"));
                    j = after_item(lines, j, indent + 4).max(next);
                    continue;
                }
                let shown = if is_struct
                    && *member_indent == indent + 4
                    && !member.starts_with("pub ")
                    && !member.starts_with("#[")
                {
                    "<private field>".to_string()
                } else {
                    member.clone()
                };
                out.push(format!(
                    "{prefix}{}{shown}",
                    " ".repeat(member_indent - indent)
                ));
                j += 1;
            }
        } else if text.starts_with("pub fn ")
            || text.starts_with("pub const ")
            || text.starts_with("pub static ")
            || text.starts_with("pub type ")
            || text.starts_with("pub use ")
            || text.starts_with("pub mod ")
            || text.starts_with("pub(crate) use ")
        {
            if text.starts_with("pub mod ") && text.ends_with('{') {
                out.push(with_attrs(text.trim_end_matches('{').trim_end()));
                let (_, body_from) = joined(lines, i);
                out.extend(outline(
                    lines,
                    body_from,
                    end,
                    indent + 4,
                    &format!("{prefix}    "),
                ));
            } else if !text.starts_with("pub(crate) use ") || !taken.is_empty() {
                // A crate-internal re-export is listed only when a `cfg`
                // decides between it and a public one.
                let declaration =
                    if text.starts_with("pub fn ") || text.starts_with("pub const fn ") {
                        joined(lines, i).0
                    } else {
                        joined_statement(lines, i)
                    };
                out.push(with_attrs(&declaration));
            }
        } else if text.starts_with("mod ") && text.ends_with('{') {
            // A private inline module: its public items may be re-exported.
            out.push(with_attrs(text.trim_end_matches('{').trim_end()));
            let (_, body_from) = joined(lines, i);
            out.extend(outline(
                lines,
                body_from,
                end,
                indent + 4,
                &format!("{prefix}    "),
            ));
        } else if text.starts_with("macro_rules! ") {
            out.push(with_attrs(text.trim_end_matches('{').trim_end()));
        } else if text.starts_with("impl") {
            let (header, body_from) = joined(lines, i);
            out.push(with_attrs(&header));
            if !header.contains(" for ") {
                out.extend(impl_members(lines, body_from, end, indent + 4, prefix));
            }
        }
        i = end.max(i + 1);
    }
    out
}

/// Public methods (and macro invocations) of an inherent `impl` block.
fn impl_members(
    lines: &[(usize, String)],
    from: usize,
    to: usize,
    indent: usize,
    prefix: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    let mut i = from;
    while i < to {
        let (ind, text) = &lines[i];
        if *ind != indent {
            i += 1;
            continue;
        }
        if text.starts_with("#[") {
            attrs.push(text.clone());
            i += 1;
            continue;
        }
        let taken = std::mem::take(&mut attrs);
        let with_attrs = |decl: &str| {
            if taken.is_empty() {
                format!("{prefix}    {decl}")
            } else {
                format!("{prefix}    {} {decl}", taken.join(" "))
            }
        };
        if text.starts_with("pub ") {
            out.push(with_attrs(&joined(lines, i).0));
        } else if text.ends_with("! {") {
            // A macro invocation: whatever it declares is listed as written.
            // (`low_level!` makes the method public with that feature.)
            let name = text.trim_end_matches(" {");
            let mut j = i + 1;
            while j < to && lines[j].1.starts_with("#[") {
                j += 1;
            }
            if j < to {
                out.push(with_attrs(&format!("{name} {}", joined(lines, j).0)));
            }
        }
        i = after_item(lines, i, indent).max(i + 1);
    }
    out
}

/// Files of modules declared without `pub` (`mod http;`): not API.
fn private_module_files(src: &Path, files: &[PathBuf]) -> BTreeSet<PathBuf> {
    let mut private = BTreeSet::new();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy();
        if name != "lib.rs" && name != "mod.rs" {
            continue;
        }
        let dir = file.parent().unwrap();
        for (indent, text) in code_lines(&std::fs::read_to_string(file).unwrap()) {
            let Some(module) = text.strip_prefix("mod ").and_then(|m| m.strip_suffix(';')) else {
                continue;
            };
            if indent == 0 {
                private.insert(dir.join(format!("{module}.rs")));
                private.insert(dir.join(module).join("mod.rs"));
            }
        }
    }
    let _ = src;
    private
}

fn features_table(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            inside = t == "[features]";
            continue;
        }
        if inside && !t.is_empty() && !t.starts_with('#') {
            out.push(t.to_string());
        }
    }
    out
}

fn listing(crate_dir: &Path) -> String {
    let src = crate_dir.join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let private = private_module_files(&src, &files);
    let mut out = String::new();

    let _ = writeln!(out, "## Cargo.toml [features]");
    let manifest = std::fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
    let features = features_table(&manifest);
    if features.is_empty() {
        let _ = writeln!(out, "(none)");
    }
    for line in features {
        let _ = writeln!(out, "{line}");
    }
    out.push('\n');

    for file in files {
        if private.contains(&file) {
            continue;
        }
        let lines = code_lines(&std::fs::read_to_string(&file).unwrap());
        let items = outline(&lines, 0, lines.len(), 0, "");
        if items.is_empty() {
            continue;
        }
        let name = file.strip_prefix(&src).unwrap().display().to_string();
        let _ = writeln!(out, "## {}", name.replace('\\', "/"));
        for line in items {
            let _ = writeln!(out, "{line}");
        }
        out.push('\n');
    }
    out
}

#[test]
fn the_public_api_matches_the_committed_outlines() {
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
                 and commit the updated outline."
            );
        }
    }
}

/// The kinds of change the outline must show, each as a small edit of the
/// same source.
#[test]
fn the_outline_reacts_to_api_changes() {
    const BASE: &str = r#"
//! Module docs.
use std::fmt;

macro_rules! low_level {
    ($x:tt) => {};
}

/// Doc.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Report {
    /// Doc.
    pub height: u32,
    pub key: [u8; 32],
    hidden: u32,
}

#[non_exhaustive]
pub enum Error {
    /// Doc.
    Unit,
    Tuple(u32),
    Range {
        from: u32,
        to: u32,
    },
}

pub trait Source {
    fn tip(&self) -> u32;
    fn stats(&self) -> u32 {
        0
    }
}

#[cfg(feature = "x")]
/// Doc between the attribute and the item.
pub fn gated(
    first: u32,
    second: &str,
) -> u32 {
    first
}

pub fn generic<T>(value: T) -> T
where
    T: Clone,
{
    value
}

pub use inner::{
    one, two,
};

pub(crate) fn internal() {}

struct Private {
    pub field_of_a_private_item: u32,
}

impl Report {
    pub fn new() -> Self {
        Self { height: 0, key: [0; 32], hidden: 0 }
    }

    low_level! {
    /// Doc.
    fn building_block(&self) -> u32 {
        1
    }
    }

    fn private_method(&self) {}
}

#[cfg(feature = "secret")]
impl Report {
    pub fn secret(&self) -> u32 {
        self.hidden
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.height)
    }
}

mod tx_level {
    pub struct Match {
        pub vout: u32,
    }
}

#[cfg(test)]
mod tests {
    pub fn not_api() {}
}

pub fn after_the_tests() {}
"#;
    let of = |source: &str| {
        let lines = code_lines(source);
        outline(&lines, 0, lines.len(), 0, "")
    };
    let base = of(BASE);
    let expected = [
        "macro_rules! low_level",
        "#[derive(Debug, Clone)] #[non_exhaustive] pub struct Report",
        "    pub height: u32,",
        "    pub key: [u8; 32],",
        "    <private field>",
        "#[non_exhaustive] pub enum Error",
        "    Unit,",
        "    Tuple(u32),",
        "    Range {",
        "        from: u32,",
        "        to: u32,",
        "    },",
        "pub trait Source",
        "    fn tip(&self) -> u32;",
        "    fn stats(&self) -> u32",
        "#[cfg(feature = \"x\")] pub fn gated( first: u32, second: &str, ) -> u32",
        "pub fn generic<T>(value: T) -> T where T: Clone,",
        "pub use inner::{ one, two, };",
        "impl Report",
        "    pub fn new() -> Self",
        "    low_level! fn building_block(&self) -> u32",
        "#[cfg(feature = \"secret\")] impl Report",
        "    pub fn secret(&self) -> u32",
        "impl fmt::Display for Report",
        "mod tx_level",
        "    pub struct Match",
        "        pub vout: u32,",
        "pub fn after_the_tests()",
    ];
    assert_eq!(base, expected, "{base:#?}");

    // Every one of these edits must change the outline.
    let edits: &[(&str, &str, &str)] = &[
        ("a field's type", "pub key: [u8; 32],", "pub key: [u8; 33],"),
        ("a private field removed", "    hidden: u32,\n", ""),
        (
            "non_exhaustive removed",
            "#[non_exhaustive]\npub struct",
            "pub struct",
        ),
        (
            "a derive added",
            "#[derive(Debug, Clone)]",
            "#[derive(Debug, Clone, Default)]",
        ),
        (
            "a variant's payload",
            "        to: u32,\n",
            "        to: u64,\n",
        ),
        (
            "a required trait method",
            "    fn tip(&self) -> u32;",
            "    fn tip(&self) -> u32;\n    fn more(&self);",
        ),
        ("a where clause", "    T: Clone,", "    T: Clone + Send,"),
        ("a re-exported name", "    one, two,", "    one,"),
        (
            "the cfg of an impl block",
            "#[cfg(feature = \"secret\")]\nimpl",
            "impl",
        ),
        (
            "a trait impl removed",
            "impl fmt::Display for Report {",
            "impl fmt::Debug for Other {",
        ),
        (
            "a method made public",
            "    fn private_method(&self) {}",
            "    pub fn private_method(&self) {}",
        ),
        (
            "an item in an inline module",
            "        pub vout: u32,",
            "        pub vout: u64,",
        ),
        (
            "a function after the test module",
            "pub fn after_the_tests() {}",
            "pub fn after_the_tests(x: u8) {}",
        ),
        (
            "a method from another macro",
            "    low_level! {",
            "    other_macro! {",
        ),
    ];
    for (what, old, new) in edits {
        assert!(BASE.contains(old), "{what}: the edit does not apply");
        assert_ne!(of(&BASE.replace(old, new)), base, "{what} went unnoticed");
    }
    // And changes that are not API leave it alone.
    for (what, old, new) in [
        ("a comment", "/// Doc.", "/// Other doc."),
        ("a function body", "    first\n", "    first + 1\n"),
        (
            "a private function",
            "pub(crate) fn internal() {}",
            "pub(crate) fn internal2() {}",
        ),
        (
            "a field of a private struct",
            "pub field_of_a_private_item: u32,",
            "pub renamed: u64,",
        ),
    ] {
        assert!(BASE.contains(old), "{what}: the edit does not apply");
        assert_eq!(
            of(&BASE.replace(old, new)),
            base,
            "{what} changed the outline"
        );
    }

    assert_eq!(
        features_table("[package]\nname = \"x\"\n\n[features]\n# comment\na = []\nb = [\"a\"]\n\n[dependencies]\nserde = \"1\"\n"),
        vec!["a = []", "b = [\"a\"]"]
    );
}
