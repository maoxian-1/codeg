"use client"

import { useCallback, useEffect, useState } from "react"
import {
  AlertTriangle,
  Check,
  Copy,
  ExternalLink,
  Eye,
  EyeOff,
  RefreshCw,
} from "lucide-react"
import { useTranslations } from "next-intl"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Switch } from "@/components/ui/switch"
import {
  startWebServer,
  stopWebServer,
  getWebServerStatus,
  getWebServiceConfig,
  updateWebServiceConfig,
  probeWebServicePort,
  type WebServerInfo,
  type WebServicePortProbe,
} from "@/lib/api"

const DEFAULT_PORT = 3080
import { openUrl } from "@/lib/platform"
import { copyTextToClipboard } from "@/lib/utils"

function AddressCard({ label, value }: { label: string; value: string }) {
  const t = useTranslations("WebServiceSettings")
  return (
    <div className="space-y-1.5">
      <div className="text-xs font-medium text-muted-foreground">{label}</div>
      <div className="group relative flex items-center rounded-md border bg-muted/40 px-3 py-2">
        <code className="min-w-0 flex-1 truncate text-sm select-all">
          {value}
        </code>
        <div className="ml-2 flex shrink-0 items-center gap-1">
          <button
            type="button"
            onClick={() => openUrl(value)}
            className="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-accent-foreground"
            title={t("open")}
          >
            <ExternalLink className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>
    </div>
  )
}

function generateRandomToken() {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID().replace(/-/g, "")
  }
  return Array.from({ length: 32 }, () =>
    Math.floor(Math.random() * 16).toString(16)
  ).join("")
}

function TokenEditor({
  label,
  value,
  onChange,
  disabled,
  placeholder,
}: {
  label: string
  value: string
  onChange: (next: string) => void
  disabled: boolean
  placeholder: string
}) {
  const t = useTranslations("WebServiceSettings")
  const [copied, setCopied] = useState(false)
  const [revealed, setRevealed] = useState(false)

  async function handleCopy() {
    if (!value) return
    const ok = await copyTextToClipboard(value)
    if (!ok) return
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }

  return (
    <div className="space-y-1.5">
      <div className="text-xs font-medium text-muted-foreground">{label}</div>
      <div className="group relative flex items-center rounded-md border bg-muted/40 px-3 py-2">
        <input
          type={revealed ? "text" : "password"}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          disabled={disabled}
          placeholder={placeholder}
          spellCheck={false}
          autoComplete="off"
          className="min-w-0 flex-1 bg-transparent font-mono text-sm outline-none placeholder:text-muted-foreground disabled:cursor-not-allowed"
        />
        <div className="ml-2 flex shrink-0 items-center gap-1">
          {!disabled && (
            <button
              type="button"
              onClick={() => onChange(generateRandomToken())}
              className="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-accent-foreground"
              title={t("regenerate")}
            >
              <RefreshCw className="h-3.5 w-3.5" />
            </button>
          )}
          <button
            type="button"
            onClick={() => setRevealed((v) => !v)}
            className="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-accent-foreground"
            title={revealed ? t("hide") : t("show")}
          >
            {revealed ? (
              <EyeOff className="h-3.5 w-3.5" />
            ) : (
              <Eye className="h-3.5 w-3.5" />
            )}
          </button>
          <button
            type="button"
            onClick={handleCopy}
            disabled={!value}
            className="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-accent-foreground disabled:cursor-not-allowed disabled:opacity-40"
            title={t("copy")}
          >
            {copied ? (
              <Check className="h-3.5 w-3.5 text-green-500" />
            ) : (
              <Copy className="h-3.5 w-3.5" />
            )}
          </button>
        </div>
      </div>
    </div>
  )
}

export function WebServiceSettings() {
  const t = useTranslations("WebServiceSettings")
  const [status, setStatus] = useState<WebServerInfo | null>(null)
  const [port, setPort] = useState(String(DEFAULT_PORT))
  const [token, setToken] = useState("")
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState("")
  const [portProbe, setPortProbe] = useState<WebServicePortProbe | null>(null)
  const [autoStart, setAutoStart] = useState(false)
  const [configLoaded, setConfigLoaded] = useState(false)

  const probePort = useCallback(async (portNum: number) => {
    try {
      const result = await probeWebServicePort(portNum)
      setPortProbe(result)
    } catch {
      setPortProbe(null)
    }
  }, [])

  const fetchStatus = useCallback(async () => {
    try {
      const fallbackConfig = {
        token: null,
        port: null,
        autoStart: false,
      }
      const [info, configResult] = await Promise.all([
        getWebServerStatus(),
        getWebServiceConfig()
          .then((config) => ({ ok: true as const, config }))
          .catch(() => ({ ok: false as const, config: fallbackConfig })),
      ])
      const savedConfig = configResult.config
      setStatus(info)
      setAutoStart(savedConfig.autoStart ?? false)
      if (info) {
        setPort(String(info.port))
        setToken(info.token)
        setPortProbe(null)
      } else {
        const resolvedPort = savedConfig.port ?? DEFAULT_PORT
        setPort(String(resolvedPort))
        if (savedConfig.token) {
          setToken(savedConfig.token)
        }
        // Detect leftover/foreign listener on the configured port so the
        // user understands why a fresh start may fail with port-in-use.
        probePort(resolvedPort)
      }
      setConfigLoaded(configResult.ok)
    } catch {
      // Server status unavailable
    }
  }, [probePort])

  useEffect(() => {
    fetchStatus()
  }, [fetchStatus])

  const persistWebServiceConfig = useCallback(
    async (nextAutoStart = autoStart) => {
      const portNum = parseInt(port, 10)
      if (!Number.isFinite(portNum) || portNum < 1 || portNum > 65535) {
        return
      }

      try {
        await updateWebServiceConfig({
          port: portNum,
          token: token.trim() || null,
          autoStart: nextAutoStart,
        })
      } catch {
        setError(t("saveConfigFailed"))
      }
    },
    [autoStart, port, t, token]
  )

  useEffect(() => {
    if (!configLoaded) return
    const portNum = parseInt(port, 10)
    if (!Number.isFinite(portNum) || portNum < 1 || portNum > 65535) {
      return
    }

    const timeout = window.setTimeout(() => {
      void persistWebServiceConfig()
    }, 500)

    return () => window.clearTimeout(timeout)
  }, [configLoaded, persistWebServiceConfig, port])

  const startErrorKeys: Record<string, string> = {
    "web_server.already_running": "errors.alreadyRunning",
    "web_server.invalid_address": "errors.invalidAddress",
    "web_server.port_in_use": "errors.portInUse",
    "web_server.permission_denied": "errors.permissionDenied",
    "web_server.address_unavailable": "errors.addressUnavailable",
    "web_server.bind_failed": "errors.bindFailed",
  }

  async function handleStart() {
    setError("")
    setLoading(true)
    try {
      const portNum = parseInt(port, 10) || DEFAULT_PORT
      const info = await startWebServer({
        port: portNum,
        token: token.trim() || null,
      })
      setStatus(info)
      setToken(info.token)
      setPort(String(info.port))
      setPortProbe(null)
    } catch (e: unknown) {
      const rawMsg =
        e && typeof e === "object" && "message" in e
          ? String((e as { message: string }).message)
          : ""
      const localKey = startErrorKeys[rawMsg]
      if (localKey) {
        setError(
          t(localKey as Parameters<typeof t>[0], {
            port: parseInt(port, 10) || DEFAULT_PORT,
          })
        )
      } else {
        setError(rawMsg || t("startFailed"))
      }
      // Refresh probe after a port_in_use failure so the banner reflects
      // current reality (e.g. confirms port really is held by another
      // process, not just a stale flag).
      if (rawMsg === "web_server.port_in_use") {
        probePort(parseInt(port, 10) || DEFAULT_PORT)
      }
    } finally {
      setLoading(false)
    }
  }

  async function handleStop() {
    setLoading(true)
    try {
      await stopWebServer()
      setStatus(null)
      // After stop, re-probe so the user can see whether the port was
      // released cleanly or is being held by an orphan child process.
      probePort(parseInt(port, 10) || DEFAULT_PORT)
    } catch {
      setError(t("stopFailed"))
    } finally {
      setLoading(false)
    }
  }

  const isRunning = status !== null
  const showStaleBanner =
    !isRunning &&
    portProbe !== null &&
    (portProbe.state === "occupied" || portProbe.state === "unknown")

  return (
    <ScrollArea className="h-full">
      <div className="space-y-6 p-3 md:p-4">
        <div>
          <h3 className="text-lg font-medium">{t("sectionTitle")}</h3>
          <p className="text-sm text-muted-foreground">
            {t("sectionDescription")}
          </p>
        </div>

        <div className="space-y-4">
          {showStaleBanner && (
            <div className="flex items-start gap-3 rounded-md border border-amber-500/40 bg-amber-500/10 p-3">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
              <div className="space-y-1 text-sm">
                <div className="font-medium text-amber-700 dark:text-amber-300">
                  {portProbe?.state === "occupied"
                    ? t("stalePortOccupiedTitle", { port: portProbe.port })
                    : t("stalePortUnknownTitle", {
                        port: portProbe?.port ?? 0,
                      })}
                </div>
                <div className="text-muted-foreground">
                  {t("stalePortHint")}
                </div>
              </div>
            </div>
          )}

          {/* Port config */}
          <div className="flex items-center gap-4">
            <label className="w-20 text-sm font-medium">{t("port")}</label>
            <input
              type="number"
              value={port}
              onChange={(e) => {
                setPort(e.target.value)
                setPortProbe(null)
              }}
              disabled={isRunning}
              min={1024}
              max={65535}
              className="flex h-9 w-32 rounded-md border border-input bg-background px-3 py-1 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
            />
          </div>

          {/* Token config */}
          <TokenEditor
            label={t("tokenLabel")}
            value={token}
            onChange={setToken}
            disabled={isRunning}
            placeholder={t("tokenPlaceholder")}
          />
          <p className="text-xs text-muted-foreground">{t("tokenHint")}</p>

          {/* Auto-start config */}
          <div className="flex items-center gap-4">
            <label className="w-20 text-sm font-medium">{t("autoStart")}</label>
            <div className="flex min-w-0 items-center gap-3">
              <Switch
                checked={autoStart}
                onCheckedChange={(checked) => {
                  setAutoStart(checked)
                  void persistWebServiceConfig(checked)
                }}
              />
              <span className="text-sm text-muted-foreground">
                {t("autoStartHint")}
              </span>
            </div>
          </div>

          {/* Start/Stop button */}
          <div className="flex items-center gap-4">
            <label className="w-20 text-sm font-medium">{t("status")}</label>
            <div className="flex items-center gap-3">
              <span
                className={`inline-block h-2 w-2 rounded-full ${
                  isRunning ? "bg-green-500" : "bg-muted-foreground/30"
                }`}
              />
              <span className="text-sm">
                {isRunning ? t("running") : t("stopped")}
              </span>
              <button
                onClick={isRunning ? handleStop : handleStart}
                disabled={loading}
                className="inline-flex h-8 items-center rounded-md border border-input bg-background px-3 text-xs font-medium ring-offset-background transition-colors hover:bg-accent hover:text-accent-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50"
              >
                {loading ? t("processing") : isRunning ? t("stop") : t("start")}
              </button>
            </div>
          </div>

          {error && <p className="text-sm text-destructive">{error}</p>}

          {/* Addresses (only when running) */}
          {isRunning && (
            <div className="space-y-3">
              {status.addresses.map((addr) => (
                <AddressCard
                  key={addr}
                  label={t("addressLabel")}
                  value={addr}
                />
              ))}
            </div>
          )}
        </div>
      </div>
    </ScrollArea>
  )
}
