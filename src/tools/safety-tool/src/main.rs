use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use quote::ToTokens;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::token::{Brace, Paren};
use syn::visit::Visit;
use syn::{Attribute, Expr, LitStr, Token, Visibility};
use walkdir::WalkDir;

fn parse_arg_value(args: &[String], name: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            return args.get(i + 1).cloned();
        }
        i += 1;
    }
    None
}

#[derive(Debug, Clone)]
struct TagDocDef {
    args: Vec<String>,
    desc: String,
}

#[derive(Debug, Clone)]
struct Annotation {
    module_path: Vec<String>,
    func_name: String,
    rendered_lines: Vec<String>,
}

#[derive(Debug, Clone)]
struct ParsedSpec {
    package_name: String,
    defs: HashMap<String, TagDocDef>,
}

fn parse_toml_defs(sp_file: &Path) -> Result<ParsedSpec, String> {
    let content = fs::read_to_string(sp_file)
        .map_err(|e| format!("failed to read sp file {}: {e}", sp_file.display()))?;
    let value: toml::Value = toml::from_str(&content)
        .map_err(|e| format!("failed to parse TOML {}: {e}", sp_file.display()))?;

    let package_name = value
        .get("package")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("core")
        .to_string();

    let mut defs = HashMap::new();
    let Some(tag_table) = value.get("tag").and_then(|v| v.as_table()) else {
        return Err("missing [tag.*] sections in sp TOML".to_string());
    };

    for (tag_name, item) in tag_table {
        let Some(item_table) = item.as_table() else {
            continue;
        };
        let args = item_table
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let Some(desc) = item_table.get("desc").and_then(|v| v.as_str()) else {
            continue;
        };
        defs.insert(
            tag_name.to_string(),
            TagDocDef {
                args,
                desc: desc.to_string(),
            },
        );
    }

    Ok(ParsedSpec { package_name, defs })
}

fn render_desc(def: &TagDocDef, values: &[String]) -> String {
    let mut rendered = def.desc.clone();
    for (i, arg_name) in def.args.iter().enumerate() {
        if let Some(value) = values.get(i) {
            rendered = rendered.replace(&format!("{{{arg_name}}}"), value);
        }
    }
    rendered
}

#[derive(Debug, Clone)]
struct TagNameType {
    typ: Option<String>,
    name: String,
}

impl Parse for TagNameType {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let first: syn::Ident = input.parse()?;
        let first = first.to_string();
        if input.peek(Token![.]) {
            let _: Token![.] = input.parse()?;
            let second: syn::Ident = input.parse()?;
            Ok(TagNameType {
                typ: Some(first),
                name: second.to_string(),
            })
        } else {
            Ok(TagNameType {
                typ: None,
                name: first,
            })
        }
    }
}

#[derive(Clone)]
struct Property {
    tag: TagNameType,
    args: Vec<Expr>,
}

impl Parse for Property {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let tag: TagNameType = input.parse()?;
        let args = if input.peek(Paren) {
            let content;
            syn::parenthesized!(content in input);
            Punctuated::<Expr, Token![,]>::parse_terminated(&content)?
                .into_iter()
                .collect()
        } else if input.peek(Brace) {
            let content;
            syn::braced!(content in input);
            Punctuated::<Expr, Token![,]>::parse_terminated(&content)?
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        Ok(Property { tag, args })
    }
}

#[derive(Clone)]
struct PropertiesAndReason {
    tags: Vec<Property>,
    desc: Option<String>,
}

impl Parse for PropertiesAndReason {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut tags = Vec::<Property>::new();
        let mut desc = None;

        while !input.is_empty() {
            let prop: Property = input.parse()?;
            tags.push(prop);

            if input.peek(Token![,]) {
                let _: Token![,] = input.parse()?;
            }

            if input.peek(Token![:]) {
                let _: Token![:] = input.parse()?;
                let s: LitStr = input.parse()?;
                desc = Some(s.value());
                break;
            }

            if input.peek(Token![;]) {
                break;
            }
        }

        Ok(PropertiesAndReason { tags, desc })
    }
}

#[derive(Clone)]
struct SafetyAttrArgs {
    groups: Vec<PropertiesAndReason>,
}

impl Parse for SafetyAttrArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        Ok(SafetyAttrArgs {
            groups: Punctuated::<PropertiesAndReason, Token![;]>::parse_terminated(input)?
                .into_iter()
                .collect(),
        })
    }
}

fn parse_safety_attr(attr: &Attribute) -> Option<SafetyAttrArgs> {
    let mut segs = attr.path().segments.iter();
    let first = segs.next()?.ident.to_string();
    let second = segs.next()?.ident.to_string();
    if segs.next().is_some() {
        return None;
    }
    if first != "safety" {
        return None;
    }
    if second != "requires" && second != "checked" {
        return None;
    }
    attr.parse_args().ok()
}

fn has_doc_hidden(attrs: &[Attribute]) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }

        let mut hidden = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("hidden") {
                hidden = true;
            }
            Ok(())
        });

        if hidden {
            return true;
        }
    }
    false
}

fn is_public_fn(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

fn expr_to_string(expr: &Expr) -> String {
    expr.to_token_stream().to_string()
}

fn fmt_tag_name(tag: &TagNameType) -> String {
    match &tag.typ {
        Some(typ) => format!("{typ}.{}", tag.name),
        None => tag.name.clone(),
    }
}

fn render_property(prop: &Property, defs: &HashMap<String, TagDocDef>) -> String {
    if prop.tag.name == "any" {
        let mut parts = Vec::new();
        for arg in &prop.args {
            let parsed = syn::parse_str::<PropertiesAndReason>(&expr_to_string(arg));
            if let Ok(group) = parsed {
                let text = render_group(&group, defs).join("; ");
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
        if parts.is_empty() {
            return "only one of the listed alternatives must hold".to_string();
        }
        return format!("only one of the following alternatives must hold: {}", parts.join(" | "));
    }

    let args = prop.args.iter().map(expr_to_string).collect::<Vec<_>>();
    let tag_name = fmt_tag_name(&prop.tag);
    if let Some(def) = defs.get(&prop.tag.name) {
        let rendered = render_desc(def, &args);
        return format!("{tag_name}: {rendered}");
    }

    if args.is_empty() {
        return tag_name;
    }

    format!("{}({})", tag_name, args.join(", "))
}

fn render_group(group: &PropertiesAndReason, defs: &HashMap<String, TagDocDef>) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(desc) = &group.desc {
        lines.push(desc.clone());
    }
    for tag in &group.tags {
        lines.push(render_property(tag, defs));
    }
    lines
}

fn module_path_from_file(src_root: &Path, file_path: &Path) -> Option<Vec<String>> {
    let rel = file_path.strip_prefix(src_root).ok()?;
    let mut comps = rel
        .iter()
        .map(|s| s.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    if comps.is_empty() {
        return Some(Vec::new());
    }

    let file = comps.pop()?;
    if file == "lib.rs" {
        return Some(comps);
    }
    if file == "mod.rs" {
        return Some(comps);
    }

    let stem = file.strip_suffix(".rs")?.to_string();
    comps.push(stem);
    Some(comps)
}

struct FileCollector<'a> {
    base_module: Vec<String>,
    module_stack: Vec<String>,
    defs: &'a HashMap<String, TagDocDef>,
    out: Vec<Annotation>,
}

impl<'a> FileCollector<'a> {
    fn full_module_path(&self) -> Vec<String> {
        let mut path = self.base_module.clone();
        path.extend(self.module_stack.iter().cloned());
        path
    }
}

impl<'ast, 'a> Visit<'ast> for FileCollector<'a> {
    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        if !is_public_fn(&i.vis) || has_doc_hidden(&i.attrs) {
            syn::visit::visit_item_fn(self, i);
            return;
        }

        let mut lines = Vec::new();
        for attr in &i.attrs {
            if let Some(parsed) = parse_safety_attr(attr) {
                for group in &parsed.groups {
                    lines.extend(render_group(group, self.defs));
                }
            }
        }

        if !lines.is_empty() {
            self.out.push(Annotation {
                module_path: self.full_module_path(),
                func_name: i.sig.ident.to_string(),
                rendered_lines: lines,
            });
        }

        syn::visit::visit_item_fn(self, i);
    }

    fn visit_item_mod(&mut self, i: &'ast syn::ItemMod) {
        if i.content.is_some() {
            self.module_stack.push(i.ident.to_string());
            syn::visit::visit_item_mod(self, i);
            self.module_stack.pop();
            return;
        }
        syn::visit::visit_item_mod(self, i);
    }
}

fn collect_annotations(src_root: &Path, defs: &HashMap<String, TagDocDef>) -> Vec<Annotation> {
    let mut out = Vec::new();

    for entry in WalkDir::new(src_root).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }

        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };

        let Ok(file) = syn::parse_file(&content) else {
            continue;
        };

        let Some(base_module) = module_path_from_file(src_root, path) else {
            continue;
        };

        let mut collector = FileCollector {
            base_module,
            module_stack: Vec::new(),
            defs,
            out: Vec::new(),
        };
        collector.visit_file(&file);
        out.extend(collector.out);
    }

    out.sort_by(|a, b| {
        a.module_path
            .cmp(&b.module_path)
            .then_with(|| a.func_name.cmp(&b.func_name))
    });
    out
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn render_html_note(lines: &[String]) -> String {
    let mut html = String::new();
    html.push_str("<div class=\"safety-tool-note\" data-safety-tool=\"true\"><p><strong>Safety requirements:</strong></p><ul>");
    for line in lines {
        html.push_str("<li>");
        html.push_str(&html_escape(line));
        html.push_str("</li>");
    }
    html.push_str("</ul></div>");
    html
}

fn inject_into_html_file(path: &Path, note_html: &str) -> std::io::Result<bool> {
    const START: &str = "<!-- safety-tool:start -->";
    const END: &str = "<!-- safety-tool:end -->";

    let original = fs::read_to_string(path)?;
    let block = format!("{START}{note_html}{END}");

    if let Some(start_pos) = original.find(START) {
        if let Some(end_rel) = original[start_pos..].find(END) {
            let end_pos = start_pos + end_rel + END.len();
            let mut updated = String::with_capacity(original.len() + block.len());
            updated.push_str(&original[..start_pos]);
            updated.push_str(&block);
            updated.push_str(&original[end_pos..]);
            if updated != original {
                fs::write(path, updated)?;
                return Ok(true);
            }
            return Ok(false);
        }
    }

    let Some(docblock_pos) = original.find("<div class=\"docblock\"") else {
        return Ok(false);
    };

    let Some(tag_end_rel) = original[docblock_pos..].find('>') else {
        return Ok(false);
    };
    let insert_at = docblock_pos + tag_end_rel + 1;
    let mut out = String::with_capacity(original.len() + block.len());
    out.push_str(&original[..insert_at]);
    out.push_str(&block);
    out.push_str(&original[insert_at..]);
    fs::write(path, out)?;
    Ok(true)
}

fn expected_fn_html_path(
    doc_root: &Path,
    crate_name: &str,
    module_path: &[String],
    fn_name: &str,
) -> PathBuf {
    let mut p = doc_root.to_path_buf();
    p.push(crate_name);
    for comp in module_path {
        p.push(comp);
    }
    p.push(format!("fn.{fn_name}.html"));
    p
}

fn inject_docs(doc_root: &Path, crate_name: &str, annotations: &[Annotation]) -> std::io::Result<usize> {
    let mut changed = 0;

    for ann in annotations {
        let rendered = render_html_note(&ann.rendered_lines);
        let expected = expected_fn_html_path(doc_root, crate_name, &ann.module_path, &ann.func_name);
        if expected.exists() {
            if inject_into_html_file(&expected, &rendered)? {
                changed += 1;
            }
        }
    }

    Ok(changed)
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();

    let src_root = parse_arg_value(&args, "--src-root")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("library/core/src"));
    let doc_root = parse_arg_value(&args, "--doc-root")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("build/safety-tool-doc"));
    let sp_file = parse_arg_value(&args, "--sp-file")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("src/tools/safety-tool/assets/sp-core.toml"));

    let spec = match parse_toml_defs(&sp_file) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("safety-tool: {e}");
            std::process::exit(1);
        }
    };

    let anns = collect_annotations(&src_root, &spec.defs);
    match inject_docs(&doc_root, &spec.package_name, &anns) {
        Ok(changed) => {
            eprintln!(
                "safety-tool: parsed {} annotations, injected {} html pages for crate {} using {}",
                anns.len(),
                changed,
                spec.package_name,
                sp_file.display()
            );
        }
        Err(err) => {
            eprintln!("safety-tool: failed to inject docs: {err}");
            std::process::exit(1);
        }
    }
}
