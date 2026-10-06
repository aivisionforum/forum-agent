// Keep focus inside settings and return it to the control that opened them.
export function focusSettings(node: HTMLElement) {
  const previous = document.activeElement as HTMLElement | null;
  const controls = () => [...node.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), summary, [tabindex="0"]')].filter(el => el.getClientRects().length > 0);
  queueMicrotask(() => (node.querySelector<HTMLElement>('[aria-current="page"]') ?? controls()[0])?.focus());
  function trap(event: KeyboardEvent) {
    if (event.key !== 'Tab') return;
    const items = controls();
    const first = items[0], last = items[items.length - 1];
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
  }
  node.addEventListener('keydown', trap);
  return {destroy() { node.removeEventListener('keydown', trap); queueMicrotask(() => previous?.focus()); }};
}
