export type ChromeTier = 0 | 1 | 2 | 3 | 4;

const ENTER_THRESHOLDS = [Number.POSITIVE_INFINITY, 420, 320, 240, 170] as const;
const EXIT_HYSTERESIS_PX = 20;

/**
 * Selects layout chrome from viewport height and the configured subtitle size.
 * It never reads or transforms caption content.
 */
export function nextChromeTier(
  current: ChromeTier,
  heightPx: number,
  fontSizePx = 24
): ChromeTier {
  const normalizedHeightPx = heightPx * (24 / Math.max(12, fontSizePx));
  let next = current;

  while (next < 4 && normalizedHeightPx < ENTER_THRESHOLDS[next + 1]) {
    next = (next + 1) as ChromeTier;
  }
  while (
    next > 0
    && normalizedHeightPx > ENTER_THRESHOLDS[next] + EXIT_HYSTERESIS_PX
  ) {
    next = (next - 1) as ChromeTier;
  }
  return next;
}
