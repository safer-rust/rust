# Safety spec assets

This directory holds example TOML files for the **`--safety-spec`** feature (unstable).

## What it does

When you document a crate, rustdoc can replace doc text of the form
`#[safety::requires(Tag(args…))]` with a single line of plain text derived from the
spec file: `Tag: <expanded description>`.

Implementation: [`passes/expand_safety_spec.rs`](../passes/expand_safety_spec.rs).

## How it runs inside rustdoc

1. **CLI** — `config.rs` parses `--safety-spec <path>` (requires `-Z unstable-options`)
   and stores it in `RenderOptions.safety_spec_file`.

2. **Load** — In `core.rs`, after the crate name is known, `load_safety_spec` reads the
   TOML. It returns `None` if `package.name` does not match the crate being documented;
   otherwise an `Arc<SafetySpec>` is stored on `DocContext::safety_spec`.

3. **Pass** — After `clean::krate`, rustdoc runs the default pass list from
   `passes/mod.rs`. `EXPAND_SAFETY_SPEC` is always scheduled (except in the separate
   `--doc-coverage` pass set). It runs **after** `PROPAGATE_DOC_CFG` and **before**
   `COLLECT_INTRA_DOC_LINKS`.

4. **Transform** — `expand_safety_spec` is a no-op if `safety_spec` is `None`. If set,
   it walks the cleaned crate (`DocFolder`) and, for items whose merged docs contain
   `#[safety::requires(`, replaces well-formed spans using the tag definitions in the
   TOML.

## Example file

`sp-core.toml` is a small sample targeting the `core` crate (`package.name = "core"`).
Use it when documenting `library/core`, for example:

```sh
RUSTDOCFLAGS='-Z unstable-options --safety-spec=/path/to/rust/src/librustdoc/assets/sp-core.toml' \
  ./x.py doc library/core
```

Adjust the path to match your checkout.

## TOML shape

- `package.name` — must equal `tcx.crate_name(LOCAL_CRATE)` for the spec to apply.
- `[tag.<Name>]` — `args` (positional names for `{name}` holes in `desc`) and `desc`
  (template string).

Placeholders in documentation use the **literal** text `#[safety::requires(Tag(a,b))]`
(on a logical line; see the module docs in `expand_safety_spec.rs` for edge cases).
