//! Expand safety documentation placeholders using a TOML specification file (`--safety-spec`).
//!
//! # Supported placeholder (only form)
//!
//! After doc fragments are merged, each **logical line** is scanned for one or more substrings of
//! the form `#[safety::requires(Tag(args...))]` (the `]` closes the `#[` attribute). The substring
//! may appear **anywhere** on the line. When the tag exists in the TOML and the argument matches
//! the tag definition, that substring is replaced by one line of plain text: `Tag: <expanded desc>`.
//!
//! For example, the following doc line:
//!
//! ```text
//! /// #[safety::requires(ValidPtrRead(dst, T, 1))]
//! ```
//!
//! will be expanded to:
//!
//! ```text
//! /// ValidPtrRead: pointer `dst` must be valid for reading the `sizeof(T)* 1` memory from it. 
//! ```
//!``
//! # TOML format
//!
//! The TOML file specifies the crate name, the tags with their descriptions and arguments.
//! ```toml
//! package.name = "core"
//!
//! [tag.ValidPtrRead]
//! args = ["p", "T", "len"]
//! desc = "pointer `{p}` must be valid for reading the `sizeof({T})* {len}` memory from it."
//! ```
//!
//! Expansion runs only when `package.name` matches the crate being documented.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use rustc_ast::token::{CommentKind, DocFragmentKind};
use rustc_data_structures::fx::FxHashMap;
use rustc_resolve::rustdoc::{DocFragment, span_of_fragments};
use rustc_span::symbol::Symbol;
use rustc_span::DUMMY_SP;
use toml::Value;

use crate::clean::{Crate, Item};
use crate::core::DocContext;
use crate::fold::DocFolder;
use crate::passes::Pass;

/// Opening text for a safety placeholder; may occur anywhere on a merged doc line.
const SAFETY_REQUIRES_PREFIX: &str = "#[safety::requires(";

pub(crate) const EXPAND_SAFETY_SPEC: Pass = Pass {
    name: "expand-safety-spec",
    run: Some(expand_safety_spec),
    description: "expand `#[safety::requires(...)]` doc lines from a `--safety-spec` TOML file",
};

/// Parsed `[tag.*]` entry: positional `args` names and `desc` template with `{name}` holes.
#[derive(Debug, Clone)]
pub(crate) struct TagDef {
    pub(crate) args: Vec<String>,
    pub(crate) desc: String,
}

#[derive(Debug, Clone)]
/// Safety spec: crate name and tags.
pub(crate) struct SafetySpec {
    /// Crate name from TOML `package.name` (must match the documented crate).
    #[allow(dead_code)]
    pub(crate) package_name: String,
    pub(crate) tags: FxHashMap<String, TagDef>,
}

/// Loads the safety spec from the given path. Returns `None` if the crate name does not match.
pub(crate) fn load_safety_spec(path: &Path, documenting_crate: &str) -> Result<Option<SafetySpec>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read safety spec {}: {e}", path.display()))?;
    let value: Value =
        toml::from_str(&content).map_err(|e| format!("failed to parse TOML {}: {e}", path.display()))?;

    let package_name = value
        .get("package")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("core")
        .to_string();

    if package_name != documenting_crate {
        return Ok(None);
    }

    let Some(tag_table) = value.get("tag").and_then(|v| v.as_table()) else {
        return Err(format!("missing `[tag.*]` tables in {}", path.display()));
    };

    let mut tags = FxHashMap::default();
    for (tag_name, item) in tag_table {
        let Some(item_table) = item.as_table() else { continue };
        let args = item_table
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let Some(desc) = item_table.get("desc").and_then(|v| v.as_str()) else { continue };
        tags.insert(
            tag_name.to_string(),
            TagDef {
                args,
                desc: desc.to_string(),
            },
        );
    }

    if tags.is_empty() {
        return Err(format!("no valid `[tag.*]` entries in {}", path.display()));
    }

    Ok(Some(SafetySpec { package_name, tags }))
}

/// Renders the description template with the given values.
///
/// # Arguments
///
/// * `def` - The tag definition.
/// * `values` - The values to substitute into the template.
///
/// # Returns
///
/// The rendered description.
///
/// # Examples
///
/// ```rust,ignore
/// let def = TagDef { args: vec!["p", "T", "len"], desc: "pointer `{p}` must be valid for reading the `sizeof({T})* {len}` memory from it" };
/// let values = vec!["dst", "i32", "1"];
/// let rendered = render_desc(&def, &values);
/// assert_eq!(rendered, "pointer `dst` must be valid for reading the `sizeof(i32)* 1` memory from it");
/// ```
fn render_desc(def: &TagDef, values: &[String]) -> String {
    let mut rendered = def.desc.clone();
    for (i, arg_name) in def.args.iter().enumerate() {
        if let Some(value) = values.get(i) {
            rendered = rendered.replace(&format!("{{{arg_name}}}"), value);
        }
    }
    rendered
}

/// Finds the corresponding closing parenthesis `)` for the opening parenthesis `(` at `open`.
/// 
/// # Arguments
///
/// * `s` - The string to search.
/// * `open` - The index of the opening parenthesis.
///
/// # Returns
///
/// The index of the closing parenthesis `)`.
///
/// # Examples
///
/// ```rust,ignore
/// let s = "(dst, T, 1)";
/// let open = 0;
/// let close = matching_paren_close(s, open);
/// assert_eq!(close, Some(10));
/// ```
fn matching_paren_close(s: &str, open: usize) -> Option<usize> {
    debug_assert_eq!(s.as_bytes().get(open).copied(), Some(b'('));
    let mut depth = 0usize;
    for (i, c) in s[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits the inner text into top-level arguments.
///
/// # Arguments
///
/// * `inner` - The inner text between the opening and closing parentheses.
///
/// # Returns
///
/// The top-level arguments.
///
/// # Examples
///
/// ```rust,ignore
/// let inner = "dst, T, 1";
/// let args = split_top_level_args(inner);
/// assert_eq!(args, ["dst", "T", "1"]);
/// ```
fn split_top_level_args(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in inner.char_indices() {
        match c {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(inner[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(inner[start..].trim().to_string());
    out
}

/// Parses the tag call from the payload.
///
/// # Arguments
///
/// * `payload` - The payload to parse.
///
/// # Returns
///
/// The tag and the top-level arguments.
///
/// # Examples
///
/// ```rust,ignore
/// let payload = "ValidPtrRead(dst, T, 1)";
/// let (tag, args) = parse_tag_call(payload).unwrap();
/// assert_eq!(tag, "ValidPtrRead");
/// assert_eq!(args, ["dst", "T", "1"]);
/// ```
fn parse_tag_call(payload: &str) -> Option<(String, Vec<String>)> {
    let payload = payload.trim();
    let open = payload.find('(')?;
    let tag = payload[..open].trim().to_string();
    if tag.is_empty() {
        return None;
    }
    let close = matching_paren_close(payload, open)?;
    let inner = payload[open + 1..close].trim();
    Some((tag, split_top_level_args(inner)))
}

/// Expands the tag call.
///
/// # Arguments
///
/// * `payload` - The payload to expand.
/// * `spec` - The safety spec.
///
/// # Returns
///
/// The expanded description.
///
/// # Examples
///
/// ```rust,ignore
/// let payload = "ValidPtrRead(dst, i32, 1)";
/// let mut spec = SafetySpec { package_name: "core".to_string(), tags: FxHashMap::default() };
/// spec.tags.insert("ValidPtrRead".to_string(), TagDef { args: vec!["p", "T", "len"], desc: "pointer `{p}` must be valid for reading the `sizeof({T})* {len}` memory from it" });
/// let expanded = expand_tag_call(payload, &spec);
/// assert_eq!(expanded, Some("ValidPtrRead: pointer `dst` must be valid for reading the `sizeof(i32)* 1` memory from it".to_string()));
/// ```
fn expand_tag_call(payload: &str, spec: &SafetySpec) -> Option<String> {
    let (tag, values) = parse_tag_call(payload)?;
    let def = spec.tags.get(&tag)?;
    if !def.args.is_empty() && values.len() != def.args.len() {
        return None;
    }
    let rendered = render_desc(def, &values);
    // Plain one-line Markdown.
    Some(format!("{tag}: {rendered}"))
}

/// `s` must start with `#[safety::requires(`.
///
/// # Returns
///
/// The byte length of the full `#[...]` span and the payload.
///
/// # Examples
///
/// ```rust,ignore
/// let s = "#[safety::requires(ValidPtrRead(dst, i32, 1))]";
/// let result = parse_requires_span_at_start(s);
/// assert_eq!(result, Some((42, "ValidPtrRead(dst, i32, 1)")));
/// ```
fn parse_requires_span_at_start(s: &str) -> Option<(usize, &str)> {
    if !s.starts_with(SAFETY_REQUIRES_PREFIX) {
        return None;
    }
    let open = SAFETY_REQUIRES_PREFIX.len() - 1;
    let close = matching_paren_close(s, open)?;
    let tail = &s[close + 1..];
    let tail_trim = tail.trim_start();
    if !tail_trim.starts_with(']') {
        return None;
    }
    let ws = tail.len() - tail_trim.len();
    let end = close + 1 + ws + 1;
    let payload = s[open + 1..close].trim();
    Some((end, payload))
}

/// Replace every well-formed `#[safety::requires(...)]` on this line when expansion succeeds.
fn replace_requires_in_line(line: &str, spec: &SafetySpec) -> String {
    if !line.contains(SAFETY_REQUIRES_PREFIX) {
        return line.to_string();
    }
    let mut result = String::with_capacity(line.len());
    let mut cursor = 0;
    while cursor < line.len() {
        let rest = &line[cursor..];
        if let Some(rel) = rest.find(SAFETY_REQUIRES_PREFIX) {
            let start = cursor + rel;
            result.push_str(&line[cursor..start]);
            let suffix = &line[start..];
            if let Some((span_len, payload)) = parse_requires_span_at_start(suffix) {
                let matched = &suffix[..span_len];
                if let Some(expanded) = expand_tag_call(payload, spec) {
                    result.push_str(&expanded);
                } else {
                    result.push_str(matched);
                }
                cursor = start + span_len;
            } else {
                result.push_str(&line[start..start + SAFETY_REQUIRES_PREFIX.len()]);
                cursor = start + SAFETY_REQUIRES_PREFIX.len();
            }
        } else {
            result.push_str(rest);
            break;
        }
    }
    result
}

/// Replace every well-formed `#[safety::requires(...)]` on this doc when expansion succeeds.
fn replace_requires_lines(doc: &str, spec: &SafetySpec) -> String {
    let mut out = String::with_capacity(doc.len());
    let mut first = true;
    for line in doc.lines() {
        if !first {
            out.push('\n');
        }
        first = false;
        if line.contains(SAFETY_REQUIRES_PREFIX) {
            out.push_str(&replace_requires_in_line(line, spec));
        } else {
            out.push_str(line);
        }
    }
    if doc.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn transform_doc(doc: &str, spec: &SafetySpec) -> String {
    replace_requires_lines(doc, spec)
}

struct SafetySpecFolder {
    spec: Arc<SafetySpec>,
}

impl DocFolder for SafetySpecFolder {
    fn fold_item(&mut self, mut item: Item) -> Option<Item> {
        if !item.attrs.doc_strings.is_empty() {
            let doc = item.doc_value();
            if doc.contains(SAFETY_REQUIRES_PREFIX) {
                let new_doc = transform_doc(&doc, &self.spec);
                if new_doc != doc {
                    let span = span_of_fragments(&item.attrs.doc_strings).unwrap_or(DUMMY_SP);
                    let frag = DocFragment {
                        span,
                        item_id: item.item_id.as_def_id(),
                        doc: Symbol::intern(&new_doc),
                        kind: DocFragmentKind::Sugared(CommentKind::Line),
                        indent: 0,
                        from_expansion: false,
                    };
                    item.inner.attrs.doc_strings = vec![frag];
                }
            }
        }
        Some(self.fold_item_recur(item))
    }
}

pub(crate) fn expand_safety_spec(krate: Crate, cx: &mut DocContext<'_>) -> Crate {
    let Some(spec) = cx.safety_spec.clone() else {
        return krate;
    };
    SafetySpecFolder { spec }.fold_crate(krate)
}
