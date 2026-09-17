/** UI results belong to the selection that requested them, even after A → B → A. */
export class SessionFence {
  private generation = 0;
  private session = '';
  private disposed = false;

  select(session: string): void {
    this.session = session;
    this.generation++;
  }

  capture(): () => boolean {
    const generation = this.generation;
    const session = this.session;
    return () => !this.disposed && this.generation === generation && this.session === session;
  }

  dispose(): void {
    this.disposed = true;
    this.generation++;
  }
}

/** Byte offsets come from the core, never JS UTF-16 indices. Fail closed on stale references. */
export function evidenceParts(text: string, start: number, end: number, quote: string): { before: string; quote: string; after: string } | null {
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || end <= start) return null;
  const bytes = new TextEncoder().encode(text);
  if (end > bytes.length) return null;
  try {
    const decoder = new TextDecoder('utf-8', { fatal: true });
    const selected = decoder.decode(bytes.slice(start, end));
    if (selected !== quote) return null;
    return { before: decoder.decode(bytes.slice(0, start)), quote: selected, after: decoder.decode(bytes.slice(end)) };
  } catch { return null; }
}

export function errorText(value: unknown): string {
  if (value instanceof Error) return value.message;
  if (typeof value === 'string') return value;
  if (value && typeof value === 'object' && 'message' in value) return String(value.message);
  return '请求失败，请刷新后重试。';
}
