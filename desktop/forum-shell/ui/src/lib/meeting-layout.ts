export const DEFAULT_CAPTION_SHARE = 0.62;
export const MEETING_SPLIT_KEY = 'forum-meeting-caption-share-v1';
export function captionShare(value: number, width: number): number {
  const available = Math.max(600, width - 8);
  const min = 280 / available, max = 1 - 300 / available;
  return Math.max(min, Math.min(max, Number.isFinite(value) ? value : DEFAULT_CAPTION_SHARE));
}
