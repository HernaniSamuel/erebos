//! Source files and byte spans into them. Every later layer refers to source
//! code only through [`Span`].

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileId(u32);

/// A half-open byte range `lo..hi` inside one source file.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: FileId,
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(file: FileId, lo: usize, hi: usize) -> Self {
        let lo = u32::try_from(lo).expect("source files are limited to 4 GiB");
        let hi = u32::try_from(hi).expect("source files are limited to 4 GiB");
        Span { file, lo, hi }
    }

    /// The smallest span covering both `self` and `other`.
    pub fn to(self, other: Span) -> Span {
        debug_assert_eq!(self.file, other.file);
        Span {
            file: self.file,
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }

    pub fn range(self) -> std::ops::Range<usize> {
        self.lo as usize..self.hi as usize
    }
}

// Compact form keeps AST/HIR dumps readable.
impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.lo, self.hi)
    }
}

impl From<Span> for miette::SourceSpan {
    fn from(span: Span) -> Self {
        (span.lo as usize, (span.hi - span.lo) as usize).into()
    }
}

pub struct SourceFile {
    pub name: String,
    pub text: String,
}

#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> FileId {
        let id = FileId(u32::try_from(self.files.len()).expect("too many source files"));
        self.files.push(SourceFile {
            name: name.into(),
            text: text.into(),
        });
        id
    }

    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn snippet(&self, span: Span) -> &str {
        &self.file(span.file).text[span.range()]
    }
}
