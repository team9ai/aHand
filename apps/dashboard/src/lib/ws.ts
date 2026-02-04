type EventHandler = (event: Record<string, unknown>) => void;

export class DashboardWS {
  private _ws: WebSocket | null = null;
  private _handlers: EventHandler[] = [];
  private _reconnectDelay = 2000;
  private _maxDelay = 30000;
  private _currentDelay = 2000;
  private _closed = false;
  private _reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private _onStatusChange: ((connected: boolean) => void) | null = null;

  connect(): void {
    if (this._closed) return;

    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    const url = `${proto}//${location.host}/dashboard/ws`;

    this._ws = new WebSocket(url);

    this._ws.onopen = () => {
      this._currentDelay = this._reconnectDelay;
      this._onStatusChange?.(true);
    };

    this._ws.onmessage = (e) => {
      try {
        const data = JSON.parse(e.data as string);
        for (const handler of this._handlers) {
          handler(data);
        }
      } catch {
        // ignore malformed JSON
      }
    };

    this._ws.onclose = () => {
      this._onStatusChange?.(false);
      if (!this._closed) {
        this._scheduleReconnect();
      }
    };

    this._ws.onerror = () => {
      this._ws?.close();
    };
  }

  /** Permanently close the connection and stop reconnecting. */
  close(): void {
    this._closed = true;
    if (this._reconnectTimer !== null) {
      clearTimeout(this._reconnectTimer);
      this._reconnectTimer = null;
    }
    this._ws?.close();
    this._ws = null;
  }

  onMessage(handler: EventHandler): void {
    this._handlers.push(handler);
  }

  onStatus(handler: (connected: boolean) => void): void {
    this._onStatusChange = handler;
  }

  get connected(): boolean {
    return this._ws?.readyState === WebSocket.OPEN;
  }

  private _scheduleReconnect(): void {
    this._reconnectTimer = setTimeout(() => {
      this._reconnectTimer = null;
      this._currentDelay = Math.min(
        this._currentDelay * 2,
        this._maxDelay,
      );
      this.connect();
    }, this._currentDelay);
  }
}
