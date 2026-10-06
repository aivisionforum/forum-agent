import type { PublicSnapshot } from './client';

/** Never keep cards visible after connection failure or a malformed/stale replacement. */
export function validatePublicSnapshot(value: unknown, minimumCursor: number): PublicSnapshot {
  if (!value || typeof value !== 'object') throw new Error('大屏快照格式无效。');
  const snapshot = value as PublicSnapshot;
  if (Object.keys(snapshot).some(key => !['cursor','artifacts','wall'].includes(key))) throw new Error('大屏快照格式无效。');
  if (snapshot.wall != null) {
    const wall = snapshot.wall;
    if (typeof wall !== 'object' || Object.keys(wall).some(key => !['phase','next_update_at_ms','server_time_ms'].includes(key))
      || !['listening','working','paused','delayed','finished'].includes(wall.phase)
      || !Number.isSafeInteger(wall.server_time_ms) || wall.server_time_ms < 0
      || (wall.next_update_at_ms !== null && (!Number.isSafeInteger(wall.next_update_at_ms) || typeof wall.next_update_at_ms !== 'number' || wall.next_update_at_ms < 0))) throw new Error('大屏运行状态格式无效。');
  }
  if (!Number.isSafeInteger(snapshot.cursor) || snapshot.cursor < minimumCursor || !Array.isArray(snapshot.artifacts)) throw new Error('大屏快照版本无效，请重新连接。');
  const allowed = new Set(['public_id','revision','kind','title','text','evidence','publication_seq']);
  const ids = new Set<string>();
  for (const item of snapshot.artifacts) {
    if (!item || typeof item !== 'object' || Object.keys(item).some(key => !allowed.has(key))
      || typeof item.public_id !== 'string' || ids.has(item.public_id) || !Number.isSafeInteger(item.revision)
      || typeof item.title !== 'string' || typeof item.text !== 'string' || !Array.isArray(item.evidence)) throw new Error('公开内容格式无效。');
    ids.add(item.public_id);
    for (const evidence of item.evidence) {
      if (!evidence || Object.keys(evidence).some(key => !['public_evidence_id','revision','text'].includes(key))
        || typeof evidence.public_evidence_id !== 'string' || typeof evidence.text !== 'string') throw new Error('公开引文格式无效。');
    }
  }
  return snapshot;
}

/** Tab-scoped storage supports refresh. The bearer is removed from the address bar. */
export function displayCredentials(url: URL, storage: Pick<Storage,'getItem'|'setItem'>): { sessionId: string; token: string; cleanUrl: string } {
  const sessionId = url.searchParams.get('session') ?? '';
  if (!/^[0-9a-f-]{36}$/i.test(sessionId)) throw new Error('请从操作台打开指定会议的大屏。');
  const key = `forum-display:${sessionId}`;
  const hash = new URLSearchParams(url.hash.slice(1));
  const supplied = hash.get('token');
  if (supplied) storage.setItem(key,supplied);
  const token = supplied ?? storage.getItem(key) ?? '';
  if (!token) throw new Error('大屏链接缺少凭据，请从操作台重新打开。');
  url.hash = '';
  return {sessionId,token,cleanUrl:url.toString()};
}

export class PublicProjection {
  cursor = 0;
  snapshot: PublicSnapshot | null = null;
  accept(value: unknown): PublicSnapshot {
    try {
      const next = validatePublicSnapshot(value,this.cursor);
      this.cursor = next.cursor;
      this.snapshot = next;
      return next;
    } catch(e) { this.disconnect(); throw e; }
  }
  disconnect(): void { this.snapshot = null; }
}
