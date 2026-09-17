//! Exact UTF-8 source coverage. Synthetic separators have no source identity.
//! Grapheme boundaries additionally keep combining marks and emoji sequences intact.
use anyhow::{ensure, Result};
use forum_contracts::SourceSpan;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinMode {
    Identity,
    Space,
    Newline,
}

impl JoinMode {
    pub fn version(self) -> &'static str {
        match self {
            Self::Identity => "identity-v1",
            Self::Space => "join-space-v1",
            Self::Newline => "join-newline-v1",
        }
    }
    fn separator(self) -> &'static str {
        match self {
            Self::Identity => "",
            Self::Space => " ",
            Self::Newline => "\n",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingPiece {
    pub input_start_utf8: usize,
    pub input_end_utf8: usize,
    pub source: Option<SourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributedText {
    pub input_text: String,
    pub source_spans: Vec<SourceSpan>,
    pub mapping: Vec<MappingPiece>,
    pub normalization_version: &'static str,
}

#[derive(Debug, Clone)]
struct Piece {
    text: String,
    source: Option<SourceSpan>,
}

#[derive(Debug, Clone)]
pub struct AttributedBuffer {
    pieces: Vec<Piece>,
    mode: JoinMode,
}

impl AttributedBuffer {
    pub fn new(mode: JoinMode) -> Self {
        Self {
            pieces: Vec::new(),
            mode,
        }
    }

    pub fn append(&mut self, span: SourceSpan) -> Result<()> {
        ensure!(
            !span.segment_id.is_nil()
                && span.start_utf8 < span.end_utf8
                && span.end_utf8 - span.start_utf8 == span.quote.len()
                && !span.quote.trim().is_empty(),
            "invalid source coverage"
        );
        ensure!(
            !self
                .pieces
                .iter()
                .filter_map(|p| p.source.as_ref())
                .any(|p| p.segment_id == span.segment_id
                    && p.segment_revision == span.segment_revision
                    && p.start_utf8 < span.end_utf8
                    && span.start_utf8 < p.end_utf8),
            "overlapping or duplicate source coverage"
        );
        if !self.pieces.is_empty() && !self.mode.separator().is_empty() {
            self.pieces.push(Piece {
                text: self.mode.separator().into(),
                source: None,
            });
        }
        self.pieces.push(Piece {
            text: span.quote.clone(),
            source: Some(span),
        });
        Ok(())
    }

    pub fn text(&self) -> String {
        self.pieces.iter().map(|p| p.text.as_str()).collect()
    }
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    pub fn snapshot(&self) -> AttributedText {
        let mut input_text = String::new();
        let mut source_spans = Vec::new();
        let mut mapping = Vec::new();
        for piece in &self.pieces {
            let start = input_text.len();
            input_text.push_str(&piece.text);
            mapping.push(MappingPiece {
                input_start_utf8: start,
                input_end_utf8: input_text.len(),
                source: piece.source.clone(),
            });
            if let Some(source) = &piece.source {
                source_spans.push(source.clone());
            }
        }
        AttributedText {
            input_text,
            source_spans,
            mapping,
            normalization_version: self.mode.version(),
        }
    }

    fn slice(&self, range: Range<usize>) -> Result<Self> {
        let text = self.text();
        ensure!(
            range.start <= range.end
                && range.end <= text.len()
                && text.is_char_boundary(range.start)
                && text.is_char_boundary(range.end),
            "split is not a UTF-8 boundary"
        );
        let boundaries = text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .collect::<Vec<_>>();
        ensure!(
            boundaries.contains(&range.start) && boundaries.contains(&range.end),
            "split would divide a grapheme"
        );
        let mut offset = 0;
        let mut out = Self::new(self.mode);
        for piece in &self.pieces {
            let start = range.start.max(offset);
            let end = range.end.min(offset + piece.text.len());
            if start < end {
                let local = start - offset..end - offset;
                let quoted = piece.text[local.clone()].to_string();
                let source = piece.source.as_ref().map(|source| SourceSpan {
                    segment_id: source.segment_id,
                    segment_revision: source.segment_revision,
                    start_utf8: source.start_utf8 + local.start,
                    end_utf8: source.start_utf8 + local.end,
                    quote: quoted.clone(),
                });
                out.pieces.push(Piece {
                    text: quoted,
                    source,
                });
            }
            offset += piece.text.len();
        }
        // A separator at the start/end of a chunk joins no two sources. It is
        // removed without pretending that any durable source bytes were lost.
        while out.pieces.first().is_some_and(|p| p.source.is_none()) {
            out.pieces.remove(0);
        }
        while out.pieces.last().is_some_and(|p| p.source.is_none()) {
            out.pieces.pop();
        }
        Ok(out)
    }

    /// Explicit trim updates every retained source byte range. The caller must
    /// account for removed source whitespace in its coverage policy; production
    /// keeps outer source whitespace and does not silently apply this transform.
    pub fn trim(&self) -> Result<Self> {
        let text = self.text();
        let start = text.len() - text.trim_start().len();
        let end = text.trim_end().len().max(start);
        self.slice(start..end)
    }

    pub fn split_at_utf8(&self, end: usize) -> Result<(Self, Self)> {
        let len = self.text().len();
        Ok((self.slice(0..end)?, self.slice(end..len)?))
    }

    /// No minimum-length threshold: a one-character unpunctuated tail remains
    /// work. Prefer an existing sentence boundary, otherwise use a grapheme cut.
    pub fn take_chunk(&mut self, max_graphemes: usize) -> Result<Option<AttributedText>> {
        ensure!(max_graphemes > 0, "chunk budget must be positive");
        if self.is_empty() {
            return Ok(None);
        }
        let text = self.text();
        let graphemes = text.grapheme_indices(true).collect::<Vec<_>>();
        let mut end = graphemes
            .get(max_graphemes)
            .map(|(i, _)| *i)
            .unwrap_or(text.len());
        if end < text.len() {
            if let Some(boundary) = graphemes
                .iter()
                .take(max_graphemes)
                .filter(|(_, g)| g.chars().any(|c| "。！？.!?;；\n".contains(c)))
                .map(|(i, g)| i + g.len())
                .last()
            {
                end = boundary;
            }
            // Keep source trailing whitespace with the preceding text, rather
            // than leaving a whitespace-only coverage fragment behind.
            for (i, g) in text[end..].grapheme_indices(true) {
                if !g.chars().all(char::is_whitespace) {
                    end += i;
                    break;
                }
                if end + i + g.len() == text.len() {
                    end = text.len();
                    break;
                }
            }
        }
        if text[..end].trim().is_empty() {
            if let Some((i, g)) = text
                .grapheme_indices(true)
                .find(|(_, g)| !g.chars().all(char::is_whitespace))
            {
                end = i + g.len();
            }
        }
        let mut offset = 0;
        for piece in &self.pieces {
            let piece_end = offset + piece.text.len();
            if piece.source.is_some() && offset < end && end < piece_end {
                let local = end - offset;
                if piece.text[..local].trim().is_empty() && offset > 0 {
                    end = offset;
                } else if piece.text[local..].trim().is_empty() {
                    end = piece_end;
                }
                break;
            }
            offset = piece_end;
        }
        let (head, tail) = self.split_at_utf8(end)?;
        let result = head.snapshot();
        ensure!(
            !result.input_text.trim().is_empty(),
            "whitespace-only chunk"
        );
        ensure!(
            result
                .source_spans
                .iter()
                .all(|s| !s.quote.trim().is_empty()),
            "whitespace-only source span"
        );
        *self = tail;
        Ok(Some(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forum_contracts::{Revision, Uuid};
    fn span(id: Uuid, text: &str, start: usize) -> SourceSpan {
        SourceSpan {
            segment_id: id,
            segment_revision: Revision::FIRST,
            start_utf8: start,
            end_utf8: start + text.len(),
            quote: text.into(),
        }
    }
    #[test]
    fn joins_have_explicit_unsourced_mapping_and_exact_offsets() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut buf = AttributedBuffer::new(JoinMode::Space);
        buf.append(span(a, "中文", 3)).unwrap();
        buf.append(span(b, "English", 0)).unwrap();
        let out = buf.snapshot();
        assert_eq!(out.input_text, "中文 English");
        assert_eq!(
            out.mapping[1],
            MappingPiece {
                input_start_utf8: 6,
                input_end_utf8: 7,
                source: None
            }
        );
        assert_eq!(out.source_spans[0].start_utf8, 3);
        assert_eq!(out.source_spans[0].end_utf8, 9);
    }
    #[test]
    fn trim_moves_utf8_ranges_without_changing_retained_text() {
        let id = Uuid::new_v4();
        let mut buf = AttributedBuffer::new(JoinMode::Identity);
        buf.append(span(id, " \t中e\u{301}📝 \n", 10)).unwrap();
        let out = buf.trim().unwrap().snapshot();
        assert_eq!(out.input_text, "中e\u{301}📝");
        assert_eq!(out.source_spans[0].start_utf8, 12);
        assert_eq!(out.source_spans[0].end_utf8, 22);
    }
    #[test]
    fn rejects_byte_and_grapheme_splits_through_emoji_or_combining_text() {
        let mut buf = AttributedBuffer::new(JoinMode::Identity);
        buf.append(span(Uuid::new_v4(), "中e\u{301}👩‍💻尾", 0))
            .unwrap();
        assert!(buf.split_at_utf8(1).is_err());
        assert!(buf.split_at_utf8(4).is_err());
        assert!(buf.split_at_utf8(10).is_err());
        let first = buf.take_chunk(2).unwrap().unwrap();
        assert_eq!(first.input_text, "中e\u{301}");
        let second = buf.take_chunk(1).unwrap().unwrap();
        assert_eq!(second.input_text, "👩‍💻");
        assert_eq!(buf.take_chunk(1).unwrap().unwrap().input_text, "尾");
        assert!(buf.is_empty());
    }
    #[test]
    fn source_split_and_multi_source_join_keep_precise_quotes() {
        let id = Uuid::new_v4();
        let mut buf = AttributedBuffer::new(JoinMode::Newline);
        buf.append(span(id, "第一句。第二句", 5)).unwrap();
        buf.append(span(Uuid::new_v4(), "Next", 0)).unwrap();
        let first = buf.take_chunk(5).unwrap().unwrap();
        assert_eq!(first.input_text, "第一句。");
        assert_eq!(first.source_spans[0].start_utf8, 5);
        assert_eq!(first.source_spans[0].end_utf8, 17);
        let rest = buf.take_chunk(100).unwrap().unwrap();
        assert_eq!(rest.input_text, "第二句\nNext");
        assert_eq!(rest.source_spans[0].start_utf8, 17);
        assert_eq!(rest.normalization_version, "join-newline-v1");
    }
    #[test]
    fn short_tail_never_waits_for_ten_chars_or_punctuation() {
        let mut buf = AttributedBuffer::new(JoinMode::Space);
        buf.append(span(Uuid::new_v4(), "不要", 0)).unwrap();
        assert_eq!(buf.take_chunk(160).unwrap().unwrap().input_text, "不要");
    }
    #[test]
    fn duplicate_and_overlap_do_not_mutate_buffer() {
        let mut buf = AttributedBuffer::new(JoinMode::Space);
        let original = span(Uuid::new_v4(), "abc", 0);
        buf.append(original.clone()).unwrap();
        let before = buf.snapshot();
        assert!(buf.append(original).is_err());
        assert_eq!(buf.snapshot(), before);
    }
    #[test]
    fn whitespace_follows_sentence_without_an_unrecoverable_tail() {
        let mut buf = AttributedBuffer::new(JoinMode::Space);
        buf.append(span(Uuid::new_v4(), "First.   Next", 0))
            .unwrap();
        let a = buf.take_chunk(7).unwrap().unwrap();
        assert_eq!(a.input_text, "First.   ");
        assert_eq!(buf.take_chunk(10).unwrap().unwrap().input_text, "Next");
    }
    #[test]
    fn leading_whitespace_from_next_source_is_not_a_separate_span() {
        let mut buf = AttributedBuffer::new(JoinMode::Space);
        buf.append(span(Uuid::new_v4(), " A ", 0)).unwrap();
        buf.append(span(Uuid::new_v4(), "  B", 0)).unwrap();
        let a = buf.take_chunk(1).unwrap().unwrap();
        let b = buf.take_chunk(1).unwrap().unwrap();
        assert_eq!(a.source_spans[0].quote, " A ");
        assert_eq!(b.source_spans[0].quote, "  B");
        assert!(buf.is_empty());
    }
}
