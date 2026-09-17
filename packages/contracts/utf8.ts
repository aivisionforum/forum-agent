import type { SourceSpan } from './forum.generated';

/** Returns an exact UTF-8 slice. This does not prove revision currentness. */
export function resolveSourceSpan(text: string, span: SourceSpan): string {
  const bytes = new TextEncoder().encode(text);
  if (!Number.isSafeInteger(span.start_utf8) || !Number.isSafeInteger(span.end_utf8)
      || span.start_utf8 < 0 || span.end_utf8 <= span.start_utf8 || span.end_utf8 > bytes.length) {
    throw new Error('Invalid source byte range');
  }
  const quote = new TextDecoder('utf-8', { fatal: true }).decode(bytes.slice(span.start_utf8, span.end_utf8));
  if (quote !== span.quote) throw new Error('Quote differs from source revision');
  return quote;
}

/** Browser selection indexes are UTF-16. Explicitly convert, never reuse them. */
export function utf16SelectionToUtf8(text: string, start: number, end: number): {start_utf8: number; end_utf8: number} {
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || end <= start || end > text.length) {
    throw new Error('Invalid text selection');
  }
  for (const boundary of [start, end]) {
    const previous = text.charCodeAt(boundary - 1), current = text.charCodeAt(boundary);
    if (previous >= 0xd800 && previous <= 0xdbff && current >= 0xdc00 && current <= 0xdfff) {
      throw new Error('Selection splits a surrogate pair');
    }
  }
  const encoder = new TextEncoder();
  return {start_utf8: encoder.encode(text.slice(0, start)).length, end_utf8: encoder.encode(text.slice(0, end)).length};
}
