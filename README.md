# html-conform-lsp

A language server that reports HTML conformance problems in the editor. It is a thin host
around [`html-conform`](https://crates.io/crates/html-conform), a pure-Rust checker whose
findings are comparable to the Nu Html Checker (vnu). No JVM, no browser, no network.

Status: prototype (0.1). It is not published on crates.io; build it from source.

## What it does

- Full document sync (`textDocument/didOpen`, `didChange`, `didClose`).
- Diagnostics from `html_conform::check_with_ranges`:
  - `code` is the rule id (for example `schema.html5`, `assertion.<id>`), `source` is
    `html-conform`, and `codeDescription.href` links to the rule guide.
  - Errors map to `Error`, warnings to `Warning`, informational findings to `Information`.
  - The range is the start tag of the element the finding is about. Findings without an end
    (all parser findings, findings about text) get an empty range at the reported point.
  - A document the checker refuses (nesting deeper than 512 levels) gets a warning on the
    first line saying it was **not checked**. Content the checker does not validate (CSS in
    `<style>`, CSP hash sources) is shown as a hint on the first line. Silence never means
    "passed" for those cases.
- Hover on a diagnostic shows the rule's description, normative source and guide link, from
  `html_conform::rules()`.
- Checks run on a separate thread after 150 ms without edits. While a check runs, further
  edits queue one new check; results for outdated text are discarded, never published.

## Limits

- Whole, complete HTML documents only. Templates (Astro, Svelte, JSX, Jinja, ...) are not
  HTML documents and will produce noise or nonsense; there is no source mapping. Use it on
  `.html` files and built output.
- Every edit re-checks the whole document; there is no incremental validation. In the
  checker's own benchmarks a typical hand-written page (up to about 150 KB) takes 15-30 ms,
  very large or generated pages up to about 170 ms.
- Ranges cover the start tag only, not an attribute, the content or the end tag, because the
  checker's parser does not record those spans.
- `html-conform` counts columns in Unicode characters. LSP wants UTF-16 code units, so the
  server converts from the finding's byte offset against the document text. A tab, `é` and an
  emoji each count as one character in the checker but as 1, 1 and 2 UTF-16 units here.
- No code actions or quick fixes, no configuration, no workspace-wide checking, no
  accessibility rules beyond what `html-conform` itself reports.
- Only stdio transport.

## Install

```sh
cargo install --git https://github.com/casoon/html-conform-lsp
```

This puts `html-conform-lsp` on your `PATH` (usually `~/.cargo/bin`). Requires Rust 1.88 or
newer.

### VS Code

There is no dedicated extension yet. Any extension that starts a language server over stdio can
run the binary: configure it with the command `html-conform-lsp` (no arguments) for the
language `html`. The setting names depend on the extension you pick.

### Neovim (0.11+)

```lua
vim.lsp.config("html_conform", {
  cmd = { "html-conform-lsp" },
  filetypes = { "html" },
  root_markers = { ".git" },
})
vim.lsp.enable("html_conform")
```

### Helix

In `~/.config/helix/languages.toml`:

```toml
[language-server.html-conform]
command = "html-conform-lsp"

[[language]]
name = "html"
language-servers = ["vscode-html-language-server", "html-conform"]
```

Adjust `language-servers` to what you already use for HTML.

## Why `lsp-server` and `lsp-types`

The server needs a read loop, a few handlers and one worker thread per check. `lsp-server`
(maintained with rust-analyzer) is a small synchronous message loop; with `lsp-types` it
brings no async runtime. `tower-lsp` would add `tokio` and `tower` for a server that spends
its time in one blocking call, and its original crate has not had a release in a long time.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
reuse lint
cargo deny check licenses
```

## License

MIT. See [LICENSE](LICENSES/MIT.txt).
