"use client"

import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react"
import { Loader2 } from "lucide-react"
import { useSearchParams } from "next/navigation"
import { useTranslations } from "next-intl"
import {
  clearRemoteDesktopTransport,
  configureRemoteDesktopTransport,
} from "@/lib/transport"
import { getRemoteWorkspaceConnection } from "@/lib/remote-workspace"
import { toErrorMessage } from "@/lib/app-error"
import type { RemoteWorkspaceConnection } from "@/lib/types"

interface RemoteConnectionContextValue {
  connection: RemoteWorkspaceConnection | null
  expired: boolean
  markExpired: () => void
}

interface RemoteConnectionState {
  connection: RemoteWorkspaceConnection | null
  loadedId: number | null
  error: string | null
  expired: boolean
}

const RemoteConnectionContext =
  createContext<RemoteConnectionContextValue | null>(null)

export function useRemoteConnection() {
  return useContext(RemoteConnectionContext)
}

export function RemoteConnectionGate({ children }: { children: ReactNode }) {
  const t = useTranslations("RemoteWorkspace")
  const searchParams = useSearchParams()
  const rawId = searchParams.get("remoteConnectionId")
  const remoteConnectionId = rawId ? Number(rawId) : null
  const [state, setState] = useState<RemoteConnectionState>({
    connection: null,
    loadedId: null,
    error: null,
    expired: false,
  })

  useEffect(() => {
    if (remoteConnectionId === null || !Number.isFinite(remoteConnectionId)) {
      clearRemoteDesktopTransport()
      return
    }

    let cancelled = false
    clearRemoteDesktopTransport()

    getRemoteWorkspaceConnection(remoteConnectionId)
      .then((next) => {
        if (cancelled) return
        configureRemoteDesktopTransport({
          id: next.id,
          name: next.name,
          baseUrl: next.base_url,
          token: next.token,
          onUnauthorized: () =>
            setState((prev) => ({ ...prev, expired: true })),
        })
        setState({
          connection: next,
          loadedId: remoteConnectionId,
          error: null,
          expired: false,
        })
      })
      .catch((err) => {
        if (cancelled) return
        clearRemoteDesktopTransport()
        setState({
          connection: null,
          loadedId: remoteConnectionId,
          error: toErrorMessage(err),
          expired: false,
        })
      })

    return () => {
      cancelled = true
    }
  }, [remoteConnectionId])

  const value = useMemo(
    () => ({
      connection: state.connection,
      expired: state.expired,
      markExpired: () => setState((prev) => ({ ...prev, expired: true })),
    }),
    [state.connection, state.expired]
  )

  const hasRemoteConnection =
    remoteConnectionId !== null && Number.isFinite(remoteConnectionId)
  const loading = hasRemoteConnection && state.loadedId !== remoteConnectionId
  const error =
    hasRemoteConnection && state.loadedId === remoteConnectionId
      ? state.error
      : null
  const expired =
    hasRemoteConnection && state.loadedId === remoteConnectionId
      ? state.expired
      : false
  const connection =
    hasRemoteConnection && state.loadedId === remoteConnectionId
      ? state.connection
      : null

  if (loading) {
    return (
      <div className="flex h-screen items-center justify-center bg-background text-sm text-muted-foreground">
        <Loader2 className="mr-2 h-4 w-4 animate-spin" />
        {t("loadingConnection")}
      </div>
    )
  }

  if (error) {
    return (
      <div className="flex h-screen items-center justify-center bg-background p-6 text-sm text-destructive">
        {t("connectionLoadFailed", { message: error })}
      </div>
    )
  }

  if (expired) {
    return (
      <div className="flex h-screen items-center justify-center bg-background p-6 text-sm text-destructive">
        {t("connectionExpired", { name: connection?.name ?? "" })}
      </div>
    )
  }

  return (
    <RemoteConnectionContext.Provider value={value}>
      {children}
    </RemoteConnectionContext.Provider>
  )
}
