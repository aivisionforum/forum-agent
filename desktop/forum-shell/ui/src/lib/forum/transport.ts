export interface ForumTransport {
  readonly mode: 'desktop' | 'display' | 'preview';
  call<T>(command: string, args?: Record<string, unknown>): Promise<T>;
}
export type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export class DesktopTransport implements ForumTransport {
  readonly mode = 'desktop' as const;
  constructor(private readonly invoke: Invoke) {}
  call<T>(command: string, args?: Record<string, unknown>): Promise<T> { return this.invoke<T>(command, args); }
}

/** The browser transport only knows the public snapshot route; no writable RPC proxy. */
export class DisplayTransport implements ForumTransport {
  readonly mode = 'display' as const;
  constructor(
    private readonly origin: string,
    private readonly token: string,
    private readonly fetcher: typeof fetch = fetch,
    private readonly timeoutMs = 8000
  ) {}
  async call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
    if (command !== 'get_public_snapshot') throw new Error('此大屏仅可读取已发布内容。');
    const sessionId = String(args.sessionId ?? '');
    if (!/^[0-9a-f-]{36}$/i.test(sessionId) || !this.token) throw new Error('缺少有效的会话或大屏访问凭据。');
    const url = new URL(`/v1/display/sessions/${encodeURIComponent(sessionId)}/snapshot`, this.origin);
    if (args.after !== null && args.after !== undefined) url.searchParams.set('after', String(args.after));
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      // WebKit requires Window as the receiver for its native fetch function.
      const response = await this.fetcher.call(globalThis, url, {
        method: 'GET', headers: { Authorization: `Bearer ${this.token}`, Accept: 'application/json' },
        signal: controller.signal, cache: 'no-store', credentials: 'omit', referrerPolicy: 'no-referrer'
      });
      if (!response.ok) throw new Error(response.status === 401 || response.status === 403
        ? '大屏链接已失效，请在操作台重新打开。' : `大屏读取失败（${response.status}）。`);
      return await response.json() as T;
    } finally { clearTimeout(timer); }
  }
}
