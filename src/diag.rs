//! Compiler diagnostics, rendered with `miette`.
//!
//! Layers push [`Diagnostic`]s into a [`Diagnostics`] sink and keep going
//! where they can, so one run reports as many errors as possible.

use std::fmt;

use miette::{
    GraphicalReportHandler, GraphicalTheme, LabeledSpan, MietteError, MietteSpanContents,
    SourceCode, SourceSpan, SpanContents,
};

use crate::span::{FileId, SourceFile, SourceMap, Span};

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

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
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
        let report = WithSource {
            diag,
            files: Files::new(diag, sources),
        };
        handler
            .render_report(&mut out, &report)
            .expect("writing to a String cannot fail");
    }
    out
}

/// The files a diagnostic's labels point into, as one source for `miette`,
/// which takes one per diagnostic: each file sits at an offset of its own,
/// one byte past the end of the one before, and a span is read, with its
/// lines, columns and name, from the file it falls in. A span over two
/// files reads nothing, so `miette` never joins the labels of two files
/// into one snippet; each file gets its own, under its name. `miette`
/// shows snippets by offset, so the file of the primary label comes
/// first, then the others in the order of their ids.
struct Files<'a> {
    /// Each file with its offset.
    files: Vec<(FileId, usize, &'a SourceFile)>,
}

impl<'a> Files<'a> {
    fn new(diag: &Diagnostic, sources: &'a SourceMap) -> Self {
        let primary = diag.labels.first().map(|l| l.span.file);
        let mut ids: Vec<FileId> = diag.labels.iter().map(|l| l.span.file).collect();
        ids.sort_by_key(|&id| (Some(id) != primary, id));
        ids.dedup();
        let mut start = 0;
        let files = ids
            .into_iter()
            .map(|id| {
                let file = sources.file(id);
                let entry = (id, start, file);
                start += file.text.len() + 1;
                entry
            })
            .collect();
        Files { files }
    }

    fn span(&self, span: Span) -> SourceSpan {
        let start = self
            .files
            .iter()
            .find(|(id, ..)| *id == span.file)
            .map(|&(_, start, _)| start)
            .expect("every label's file is among the files");
        (start + span.lo as usize, (span.hi - span.lo) as usize).into()
    }
}

impl SourceCode for Files<'_> {
    fn read_span<'b>(
        &'b self,
        span: &SourceSpan,
        context_lines_before: usize,
        context_lines_after: usize,
    ) -> Result<Box<dyn SpanContents<'b> + 'b>, MietteError> {
        let (start, file) = self
            .files
            .iter()
            .map(|&(_, start, file)| (start, file))
            .find(|&(start, file)| {
                span.offset() >= start && span.offset() + span.len() <= start + file.text.len()
            })
            .ok_or(MietteError::OutOfBounds)?;
        let local = SourceSpan::from((span.offset() - start, span.len()));
        let contents =
            file.text
                .as_str()
                .read_span(&local, context_lines_before, context_lines_after)?;
        Ok(Box::new(MietteSpanContents::new_named(
            file.name.clone(),
            contents.data(),
            (start + contents.span().offset(), contents.span().len()).into(),
            contents.line(),
            contents.column(),
            contents.line_count(),
        )))
    }
}

/// Attaches source text to a diagnostic for `miette` rendering.
struct WithSource<'a> {
    diag: &'a Diagnostic,
    files: Files<'a>,
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
        (!self.files.files.is_empty()).then_some(&self.files as &dyn miette::SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        if self.diag.labels.is_empty() {
            return None;
        }
        let labels = self.diag.labels.iter().enumerate().map(|(i, l)| {
            let text = (!l.message.is_empty()).then(|| l.message.clone());
            let span = self.files.span(l.span);
            if i == 0 {
                LabeledSpan::new_primary_with_span(text, span)
            } else {
                LabeledSpan::new_with_span(text, span)
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

    /// Each label is shown in its own file, with that file's lines, even
    /// where the lines of the two files would overlap.
    #[test]
    fn renders_labels_in_several_files() {
        let mut sources = SourceMap::default();
        let main = sources.add("main.ers", "fn main() {}\nfn other() {}\n");
        let module = sources.add("a/b.ers", "const X: u8 = 1;\n");
        let diag = Diagnostic::error("test::example", "two files")
            .label(Span::new(main, 16, 21), "used here")
            .label(Span::new(module, 6, 7), "declared here");

        let out = render(&[diag], &sources, false);

        assert!(out.contains("╭─[main.ers:2:4]"), "{out}");
        assert!(out.contains("╭─[a/b.ers:1:7]"), "{out}");
        assert!(out.contains(" 2 │ fn other() {}"), "{out}");
        assert!(out.contains(" 1 │ const X: u8 = 1;"), "{out}");
        let main_at = out.find("main.ers").unwrap();
        assert!(out.find("a/b.ers").unwrap() > main_at, "{out}");
    }

    /// The snippet of the primary label comes first, even when its file
    /// comes after another label's.
    #[test]
    fn renders_the_primary_file_first() {
        let mut sources = SourceMap::default();
        let main = sources.add(
            "main.ers",
            "fn main() {}
",
        );
        let module = sources.add(
            "m.ers",
            "const X: u8 = 1;
",
        );
        let diag = Diagnostic::error("test::example", "two files")
            .label(Span::new(module, 6, 7), "failed here")
            .label(Span::new(main, 3, 7), "called here");

        let out = render(&[diag], &sources, false);

        let module_at = out.find("╭─[m.ers:1:7]").expect(&out);
        assert!(
            out.find("╭─[main.ers:1:4]").expect(&out) > module_at,
            "{out}"
        );
    }
}
