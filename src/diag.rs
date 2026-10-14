//! Compiler diagnostics, rendered with `miette`.
//!
//! Layers push [`Diagnostic`]s into a [`Diagnostics`] sink and keep going
//! where they can, so one run reports as many errors as possible.

use std::fmt;

use miette::{GraphicalReportHandler, GraphicalTheme, LabeledSpan, NamedSource};

use crate::span::{SourceMap, Span};

#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// Stable identifier, e.g. `parser::expected-token`.
    pub code: &'static str,
    pub message: String,
    /// The first label is the primary one.
    pub labels: Vec<Label>,
    pub help: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Diagnostic {
            code,
            message: message.into(),
            labels: Vec::new(),
            help: None,
        }
    }

    pub fn label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

#[derive(Default, Debug)]
pub struct Diagnostics {
    list: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn push(&mut self, diag: Diagnostic) {
        self.list.push(diag);
    }

    pub fn has_errors(&self) -> bool {
        !self.list.is_empty()
    }

    pub fn append(&mut self, other: Diagnostics) {
        self.list.extend(other.list);
    }

    /// The diagnostics in source order: by the position of their primary
    /// label, in order of emission at the same position, unlabeled ones
    /// last. Layers run on demand (ADR 0028), so emission order depends
    /// on the schedule; this order does not.
    pub fn into_vec(mut self) -> Vec<Diagnostic> {
        self.list.sort_by_key(|diag| {
            let at = diag
                .labels
                .first()
                .map(|label| (label.span.file, label.span.lo));
            (at.is_none(), at)
        });
        self.list
    }
}

/// Renders diagnostics as text. `color: false` gives stable output for tests.
pub fn render(diags: &[Diagnostic], sources: &SourceMap, color: bool) -> String {
    let theme = if color {
        GraphicalTheme::unicode()
    } else {
        GraphicalTheme::unicode_nocolor()
    };
    let handler = GraphicalReportHandler::new_themed(theme).with_width(100);

    let mut out = String::new();
    for diag in diags {
        // Labels of one diagnostic always point into the same file.
        let file = diag.labels.first().map(|l| sources.file(l.span.file));
        let source = file.map(|f| NamedSource::new(&f.name, f.text.clone()));
        let report = WithSource { diag, source };
        handler
            .render_report(&mut out, &report)
            .expect("writing to a String cannot fail");
    }
    out
}

/// Attaches source text to a diagnostic for `miette` rendering.
struct WithSource<'a> {
    diag: &'a Diagnostic,
    source: Option<NamedSource<String>>,
}

impl fmt::Debug for WithSource<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.diag, f)
    }
}

impl fmt::Display for WithSource<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.diag.message)
    }
}

impl std::error::Error for WithSource<'_> {}

impl miette::Diagnostic for WithSource<'_> {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.diag.code))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.diag
            .help
            .as_ref()
            .map(|h| Box::new(h) as Box<dyn fmt::Display>)
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.source.as_ref().map(|s| s as &dyn miette::SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        if self.diag.labels.is_empty() {
            return None;
        }
        let labels = self.diag.labels.iter().enumerate().map(|(i, l)| {
            let text = (!l.message.is_empty()).then(|| l.message.clone());
            if i == 0 {
                LabeledSpan::new_primary_with_span(text, l.span)
            } else {
                LabeledSpan::new_with_span(text, l.span)
            }
        });
        Some(Box::new(labels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_come_out_in_source_order() {
        let mut sources = SourceMap::default();
        let file = sources.add(
            "test.ers",
            "fn main() {}
",
        );
        let mut diags = Diagnostics::default();
        diags.push(Diagnostic::error("test::unlabeled", "no position"));
        diags.push(Diagnostic::error("test::late", "late").label(Span::new(file, 9, 10), ""));
        diags.push(Diagnostic::error("test::early", "early").label(Span::new(file, 3, 7), ""));
        diags.push(Diagnostic::error("test::early-too", "same").label(Span::new(file, 3, 4), ""));
        let codes: Vec<_> = diags.into_vec().iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            [
                "test::early",
                "test::early-too",
                "test::late",
                "test::unlabeled"
            ]
        );
    }

    #[test]
    fn renders_code_message_label_and_help() {
        let mut sources = SourceMap::default();
        let file = sources.add("test.ers", "fn main() {}\n");
        let diag = Diagnostic::error("test::example", "something is wrong")
            .label(Span::new(file, 3, 7), "here")
            .help("try something else");

        let out = render(&[diag], &sources, false);

        assert!(out.contains("test::example"), "{out}");
        assert!(out.contains("something is wrong"), "{out}");
        assert!(out.contains("here"), "{out}");
        assert!(out.contains("try something else"), "{out}");
        assert!(out.contains("test.ers:1:4"), "{out}");
    }
}
