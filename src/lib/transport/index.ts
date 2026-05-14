import { detectEnvironment } from "./detect"
import type { RemoteTransportConfig, Transport } from "./types"

export type { RemoteTransportConfig, Transport, UnsubscribeFn } from "./types"

let _transport: Transport | null = null
let _shellTransport: Transport | null = null
let _remoteConfig: RemoteTransportConfig | null = null

function createTauriTransport(): Transport {
  // Use dynamic require to avoid bundling tauri deps in web mode.
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const { TauriTransport } = require("./tauri-transport") as {
    TauriTransport: new () => Transport
  }
  return new TauriTransport()
}

function createWebTransport(baseUrl: string): Transport {
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const { WebTransport } = require("./web-transport") as {
    WebTransport: new (baseUrl: string) => Transport
  }
  return new WebTransport(baseUrl)
}

export function getShellTransport(): Transport {
  if (!_shellTransport) {
    const env = detectEnvironment()
    _shellTransport =
      env === "tauri"
        ? createTauriTransport()
        : createWebTransport(window.location.origin)
  }
  return _shellTransport
}

export function configureRemoteDesktopTransport(
  config: RemoteTransportConfig
): void {
  _transport?.destroy?.()
  _remoteConfig = config
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const { RemoteDesktopTransport } = require("./remote-desktop-transport") as {
    RemoteDesktopTransport: new (config: RemoteTransportConfig) => Transport
  }
  _transport = new RemoteDesktopTransport(config)
}

export function clearRemoteDesktopTransport(): void {
  _transport?.destroy?.()
  _transport = null
  _remoteConfig = null
}

export function getActiveRemoteConnectionId(): number | null {
  return _remoteConfig?.id ?? null
}

export function getTransport(): Transport {
  if (_transport) return _transport

  const env = detectEnvironment()
  _transport =
    env === "tauri"
      ? getShellTransport()
      : createWebTransport(window.location.origin)
  return _transport
}

export function isDesktop(): boolean {
  return detectEnvironment() === "tauri"
}
