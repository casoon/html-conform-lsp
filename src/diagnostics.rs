// SPDX-FileCopyrightText: 2026 CASOON
// SPDX-License-Identifier: MIT

//! Runs `html-conform` over a document and maps the result to LSP diagnostics.

use a11y_report::{Outcome, SourcePosition};
use html_conform::{CheckOptions, check_with_ranges};
use lsp_types::{
    CodeDescription, Diagnostic, DiagnosticSeverity, NumberOrString, Position, Range, Uri,
};

use crate::position::LineIndex;

pub const SOURCE: &str = "html-conform";
/// Code of the diagnostic that says the document was not checked at all.
pub const NOT_CHECKED: &str = "html-conform.not-checked";
/// Code of the hint that says a class of content was left unchecked.
pub const SKIPPED: &str = "html-conform.skipped";

/// Checks `text` as a complete HTML document. Blocking; call it off the main thread.
pub fn diagnose(text: &str) -> Vec<Diagnostic> {
    let index = LineIndex::new(text);
    let ranged = match check_with_ranges(text, CheckOptions::default()) {
        Ok(ranged) => ranged,
        // "Did not run" must not look like "passed".
        Err(error) => {
            return vec![not_checked(text, &error.to_string())];
        }
    };

    let report = ranged.to_report();
    let mut diagnostics: Vec<Diagnostic> = report
        .findings
        .iter()
        .map(|finding| {
            let severity = match finding.outcome {
                Outcome::Fail => DiagnosticSeverity::ERROR,
                Outcome::Review => DiagnosticSeverity::WARNING,
                _ => DiagnosticSeverity::INFORMATION,
            };
            let range = match finding.location.position {
                Some(start) => {
                    let start = to_lsp(&index, start);
                    let end = finding
                        .location
                        .end
                        .map_or(start, |end| to_lsp(&index, end));
                    Range::new(start, end)
                }
                None => Range::new(Position::new(0, 0), index.line_end(0)),
            };
            Diagnostic {
                range,
                severity: Some(severity),
                code: Some(NumberOrString::String(finding.rule_id.clone())),
                code_description: finding
                    .help_url
                    .as_deref()
                    .and_then(|href| href.parse::<Uri>().ok())
                    .map(|href| CodeDescription { href }),
                source: Some(SOURCE.to_owned()),
                message: finding.message.clone(),
                ..Diagnostic::default()
            }
        })
        .collect();

    for note in &report.inputs {
        diagnostics.push(first_line(
            &index,
            DiagnosticSeverity::HINT,
            SKIPPED,
            format!(
                "Not checked: {}{}",
                note.subject.as_deref().unwrap_or("part of the document"),
                note.detail
                    .as_deref()
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default()
            ),
        ));
    }
    diagnostics
}

/// The diagnostic that says the document was not checked, on its first line.
pub fn not_checked(text: &str, reason: &str) -> Diagnostic {
    first_line(
        &LineIndex::new(text),
        DiagnosticSeverity::WARNING,
        NOT_CHECKED,
        format!("Document not checked: {reason}"),
    )
}

fn to_lsp(index: &LineIndex, position: SourcePosition) -> Position {
    index.position(position.byte_offset)
}

fn first_line(
    index: &LineIndex,
    severity: DiagnosticSeverity,
    code: &str,
    message: String,
) -> Diagnostic {
    Diagnostic {
        range: Range::new(Position::new(0, 0), index.line_end(0)),
        severity: Some(severity),
        code: Some(NumberOrString::String(code.to_owned())),
        source: Some(SOURCE.to_owned()),
        message,
        ..Diagnostic::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(d: &Diagnostic) -> &str {
        match d.code.as_ref().unwrap() {
            NumberOrString::String(s) => s,
            NumberOrString::Number(_) => unreachable!(),
        }
    }

    #[test]
    fn conforming_document_has_no_diagnostics() {
        let doc = "<!DOCTYPE html>\n<html lang=\"en\"><head><title>t</title></head><body><p>x</p></body></html>\n";
        assert_eq!(diagnose(doc), vec![]);
    }

    #[test]
    fn start_tag_range_with_utf16_columns() {
        // The emoji makes the UTF-16 column differ from the char column; the `<img>` has no alt.
        let doc = "<!DOCTYPE html>\n<html lang=\"en\"><head><title>t</title></head><body><p>😀 é</p>\n<p>😀</p><img src=\"a.png\"></body></html>\n";
        let diagnostics = diagnose(doc);
        let img = diagnostics
            .iter()
            .find(|d| d.message.contains("alt"))
            .unwrap_or_else(|| panic!("no alt finding in {diagnostics:#?}"));
        let line = "<p>😀</p>";
        let expected = line.encode_utf16().count() as u32;
        assert_eq!(img.range.start, Position::new(2, expected));
        // End is the exclusive end of the start tag `<img src="a.png">`.
        assert_eq!(img.range.end.character - img.range.start.character, 17);
        assert_eq!(img.source.as_deref(), Some("html-conform"));
        assert_eq!(img.severity, Some(DiagnosticSeverity::ERROR));
        assert!(code(img).starts_with("schema.") || code(img).starts_with("assertion."));
        assert!(img.code_description.is_some());
    }

    #[test]
    fn too_deep_document_is_not_checked_not_passed() {
        let doc = "<div>".repeat(600);
        let diagnostics = diagnose(&doc);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(code(&diagnostics[0]), NOT_CHECKED);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::WARNING));
    }

    #[test]
    fn style_element_is_reported_as_unchecked_hint() {
        let doc = "<!DOCTYPE html>\n<html lang=\"en\"><head><title>t</title><style>p{}</style></head><body><p>x</p></body></html>\n";
        let diagnostics = diagnose(doc);
        let hint = diagnostics.iter().find(|d| code(d) == SKIPPED).unwrap();
        assert_eq!(hint.severity, Some(DiagnosticSeverity::HINT));
    }
}
