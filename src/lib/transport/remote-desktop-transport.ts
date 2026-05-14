import type { RemoteTransportConfig, Transport, UnsubscribeFn } from "./types"
import { buildCodegWebSocketProtocols } from "./ws-auth"

const REMOTE_CALL_TIMEOUT_MS = 30_000

interface WebEvent {
  channel: string
  payload: unknown
}

export class RemoteDesktopTransport implements Transport {
  private ws: WebSocket | null = null
  private handlers = new Map<string, Set<(payload: unknown) => void>>()
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null
  private wsFailCount = 0
  private config: RemoteTransportConfig

  constructor(config: RemoteTransportConfig) {
    this.config = {
      ...config,
      baseUrl: config.baseUrl.replace(/\/+$/, ""),
    }
  }

  async call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    const controller = new AbortController()
    const timeout = window.setTimeout(
      () => controller.abort(),
      REMOTE_CALL_TIMEOUT_MS
    )
    let res: Response
    try {
      res = await fetch(`${this.config.baseUrl}/api/${command}`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${this.config.token}`,
        },
        body: JSON.stringify(args ?? {}),
        signal: controller.signal,
      })
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") {
        throw new Error("Remote Workspace request timed out")
      }
      throw err
    } finally {
      window.clearTimeout(timeout)
    }
    if (res.status === 401) {
      this.config.onUnauthorized?.()
      throw new Error("Remote Workspace connection expired")
    }
    if (!res.ok) {
      const error = await res.json().catch(() => ({
        code: "network_error",
        message: `HTTP ${res.status}`,
      }))
      throw error
    }
    return res.json()
  }

  async subscribe<T>(
    event: string,
    handler: (payload: T) => void
  ): Promise<UnsubscribeFn> {
    if (!this.handlers.has(event)) {
      this.handlers.set(event, new Set())
    }
    const wrappedHandler = handler as (payload: unknown) => void
    this.handlers.get(event)!.add(wrappedHandler)

    if (!this.ws) {
      this.connectWs()
    }

    return () => {
      this.handlers.get(event)?.delete(wrappedHandler)
    }
  }

  isDesktop(): boolean {
    return true
  }

  private connectWs() {
    const wsUrl = this.config.baseUrl.replace(/^http/, "ws") + "/ws/events"
    this.ws = new WebSocket(
      wsUrl,
      buildCodegWebSocketProtocols(this.config.token)
    )

    this.ws.onopen = () => {
      this.wsFailCount = 0
    }

    this.ws.onmessage = (msg) => {
      try {
        const event = JSON.parse(msg.data) as WebEvent
        const handlers = this.handlers.get(event.channel)
        if (handlers) {
          for (const h of handlers) h(event.payload)
        }
      } catch {
        return
      }
    }

    this.ws.onclose = () => {
      this.ws = null
      this.wsFailCount++
      if (this.wsFailCount >= 3) {
        this.config.onUnauthorized?.()
        return
      }
      this.reconnectTimer = setTimeout(() => this.connectWs(), 3000)
    }

    this.ws.onerror = () => {
      this.ws?.close()
    }
  }

  destroy() {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    this.ws?.close()
    this.ws = null
    this.handlers.clear()
  }
}
