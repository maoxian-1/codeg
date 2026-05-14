export type UnsubscribeFn = () => void

export interface RemoteTransportConfig {
  id: number
  name: string
  baseUrl: string
  token: string
  onUnauthorized?: () => void
}

export interface Transport {
  /**
   * Invoke a backend command (replaces Tauri's invoke()).
   */
  call<T>(command: string, args?: Record<string, unknown>): Promise<T>

  /**
   * Subscribe to a backend event stream (replaces Tauri's listen()).
   * Returns an unsubscribe function.
   */
  subscribe<T>(
    event: string,
    handler: (payload: T) => void
  ): Promise<UnsubscribeFn>

  /**
   * Whether the app is running in a desktop Tauri environment.
   */
  isDesktop(): boolean

  destroy?(): void
}
