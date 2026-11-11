# Changelog

All notable changes to html-conform-lsp are listed here. html-conform-lsp follows
[Semantic Versioning](https://semver.org/); before 1.0, a minor version may change behavior.

## [Unreleased]

### Added

- **Server:** first prototype. Full-sync `textDocument/didOpen`, `didChange` and `didClose` for
  whole HTML documents; debounced checks (150 ms) off the main thread that drop results for
  outdated text.
- **Diagnostics:** findings of `html-conform` with start-tag ranges, UTF-16 columns, `code` set
  to the rule id, `source` `html-conform` and `codeDescription` linking to the rule guide.
  A document that could not be checked, and content the checker skips, are reported instead of
  being shown as clean.
- **Hover:** description, normative source and guide link of the rule behind a diagnostic.
