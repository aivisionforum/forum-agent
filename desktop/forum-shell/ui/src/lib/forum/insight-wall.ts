import type { PublicArtifact, PublicSnapshot } from './client';

export type WallView = 'live' | 'recap';
export function wallArtifacts(snapshot: PublicSnapshot | null, view: WallView): PublicArtifact[] {
  const insights = (snapshot?.artifacts ?? []).filter(a => a.kind === 'insight')
    .sort((a,b) => a.publication_seq - b.publication_seq);
  return view === 'recap' ? insights : insights.slice(-1);
}

/** Measure rendered text, splitting even one long card; preserve all Unicode text. */
export function paginateText(text: string, fits: (text: string) => boolean): string[] {
  const chars = Array.from(new Intl.Segmenter(undefined,{granularity:'grapheme'}).segment(text), part => part.segment);
  const pages: string[] = [];
  let offset = 0;
  while (offset < chars.length) {
    let low = 1, high = chars.length - offset, count = 0;
    while (low <= high) {
      const middle = Math.floor((low + high) / 2);
      if (fits(chars.slice(offset,offset + middle).join(''))) { count = middle; low = middle + 1; }
      else high = middle - 1;
    }
    if (!count) throw new Error('屏幕可用空间不足，请退出浏览器缩放或扩大窗口。');
    // Prefer a nearby paragraph/word/sentence boundary, without losing text.
    if (offset + count < chars.length) {
      for (let i = count - 1; i >= Math.floor(count * .65); i--) {
        if (/[\s。！？.!?]/u.test(chars[offset+i])) { count = i + 1; break; }
      }
    }
    pages.push(chars.slice(offset,offset+count).join(''));
    offset += count;
  }
  return pages;
}

export function wallMessage(snapshot: PublicSnapshot | null, now: number): {label: string; detail: string} {
  const wall = snapshot?.wall;
  if (!wall) return {label:'正在同步',detail:'等待本场新的讨论要点'};
  if (wall.phase === 'working') return {label:'WORKING',detail:'正在整理本轮洞察'};
  if (wall.phase === 'paused') return {label:'稍候继续',detail:'字幕优先，正在等待整理'};
  if (wall.phase === 'delayed') return {label:'本轮更新延迟',detail:'请操作员查看控制台；已有要点继续保留'};
  if (wall.phase === 'finished') return {label:'本场已结束',detail:'可切换到“本场至今”回顾讨论'};
  if (wall.next_update_at_ms == null) return {label:'等待本场开始',detail:'开始后约每 3 分钟整理一轮'};
  const seconds = Math.max(0,Math.ceil((wall.next_update_at_ms-now)/1000));
  if (!seconds) return {label:'等待新一轮整理',detail:'等待新增发言或本地模型就绪'};
  return {label:`${Math.floor(seconds/60)}:${String(seconds%60).padStart(2,'0')}`,detail:'距下一轮整理 · 约 3 分钟一轮'};
}
