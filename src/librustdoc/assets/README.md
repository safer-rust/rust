# Safety spec assets

This directory holds example TOML files for the **`--safety-spec`** feature (unstable).

## What it does

When you document a crate, rustdoc can read **`#[safety::requires(…)]`** attributes on items (functions, methods, structs, etc.), parse the **safety tag** attributes to generate corresponding Markdown description. Then rustdoc can **merge** generated Markdown into that item’s documentation. Safety tags are **comma-separated** inside `requires(...)`.

Each **safety tag** becomes one list line:

- **`Tag(arg1, arg2, …)`** — `* Tag: <text>` from the TOML `[tag.Tag]` template (`desc` with `{arg}` placeholders).
- **`Tag = "…"`** with **`Tag` present in the TOML** — `* Tag: …` with literal string as description at this place.
- **`Tag = "…"`** with **`Tag` absent from the TOML** — `* <Label>: …` where `<Label>` is a customized `Tag` with formatting(replace all underscores (`_`) with spaces and capitalize the first letter).

Templates and inline strings may include Markdown and intra-doc links (e.g. `[text](crate::path)`); injection runs **before** intra-doc link collection so those links are resolved like normal doc comments.

Implementation: [`passes/inject_safety_docs.rs`](../passes/inject_safety_docs.rs).

## How it runs inside rustdoc

1. **CLI** — `config.rs` parses `--safety-spec <path>` (unstable; use **`-Z unstable-options`** when invoking rustdoc directly) and stores it in `RenderOptions.safety_spec`.

2. **Load** — In `core.rs`, after the crate name is known, `load_safety_spec` reads the TOML. It returns `None` if `package.name` does not match the crate being documented (or the file is invalid); otherwise an `Arc<SafetySpec>` is stored on `DocContext::safety_spec`.

3. **Pass** — After `clean::krate`, rustdoc runs the default pass list from `passes/mod.rs`. **`INJECT_SAFETY_DOCS`** is always scheduled in `DEFAULT_PASSES`. It runs **before** `COLLECT_INTRA_DOC_LINKS`.

4. **Transform** — `inject_safety_docs` is a no-op if `safety_spec` is `None`. If set, it walks the cleaned crate (`DocFolder`), and for supported items it collects `#[safety::requires]` from attributes: `Tag(args)` use TOML `desc` + args; `Tag = "…"` clauses use the literal text; then it builds the list Markdown and splices it into the merged doc string (typically under **# Safety**; see `inject_safety_markdown` in the same module).

## Example file

`sp-core.toml` is a small sample targeting the `core` crate (`package.name = "core"`).
Use it when documenting `library/core`, for example:

```sh
RUSTDOCFLAGS='-Z unstable-options --safety-spec=/path/to/rust/src/librustdoc/assets/sp-core.toml' \
  ./x.py doc library/core --stage 1
```

Adjust the path to match your checkout.

## TOML shape

- `package.name` — must equal `tcx.crate_name(LOCAL_CRATE)` for the spec to apply.
- `[tag.<Name>]` — `args` (positional names for `{name}` holes in `desc`) and `desc`
  (template string).

Attributes use the real Rust attribute syntax **`#[safety::requires(Tag(a, b), …)]`** on the item. The crate must resolve the `safety` tool path (for `core`, see `#![register_tool(safety)]` in `library/core/src/lib.rs`). Multiple comma-separated clauses in one `requires(...)` are supported.
