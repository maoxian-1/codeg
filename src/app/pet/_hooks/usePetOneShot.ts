"use client"

import { useEffect, useRef, useState } from "react"
import { isDesktop } from "@/lib/transport"
import type { PetState } from "@/lib/pet/animation"

const PET_ONESHOT_EVENT = "pet://oneshot"

// Subset of `PetState` rows that backend emits as oneshots. The mapper and
// `pet_celebrate` only ever push `jumping`, `waving`, or `failed`; the
// ambient rows (`idle`, `running`, `waiting`, `review`, `running_left/right`)
// belong to `pet://state` and are intentionally excluded here so the
// renderer never plays an ambient row as a one-shot override.
export type PetOneShotKind = "jumping" | "waving" | "failed"

const ONESHOT_KINDS: ReadonlySet<string> = new Set([
  "jumping",
  "waving",
  "failed",
])

export interface PetOneShotEvent {
  kind: PetOneShotKind
  // Monotonic counter so consumers can re-trigger animations even when the
  // backend sends the same kind back-to-back. Two `jumping` oneshots in a
  // row should produce two animations, not be deduplicated by React.
  key: number
}

export function usePetOneShot(): PetOneShotEvent | null {
  const [event, setEvent] = useState<PetOneShotEvent | null>(null)
  // Survives strict-mode mount → unmount → remount so the key really is
  // monotonic across the component's lifetime. A useEffect-local counter
  // would reset to 1 on every effect re-run, breaking the "always
  // re-trigger downstream effects" contract that PetWindow depends on.
  const keyRef = useRef(0)

  useEffect(() => {
    let unlisten: (() => void) | null = null
    let cancelled = false

    function deliver(payload: unknown) {
      const kind = normalize(payload)
      if (!kind) return
      keyRef.current += 1
      setEvent({ kind, key: keyRef.current })
    }

    async function subscribe() {
      try {
        if (isDesktop()) {
          const { listen } = await import("@tauri-apps/api/event")
          const off = await listen<PetState>(PET_ONESHOT_EVENT, (e) => {
            if (cancelled) return
            deliver(e.payload)
          })
          if (cancelled) {
            off()
          } else {
            unlisten = off
          }
        } else {
          const { getTransport } = await import("@/lib/transport")
          const off = await getTransport().subscribe<PetState>(
            PET_ONESHOT_EVENT,
            (payload) => {
              if (cancelled) return
              deliver(payload)
            }
          )
          if (cancelled) {
            off()
          } else {
            unlisten = off
          }
        }
      } catch (err) {
        // Subscription failures are non-fatal — pet just stays in its
        // ambient state without celebration animations.
        console.warn("[Pet] oneshot subscription failed:", err)
      }
    }

    void subscribe()

    return () => {
      cancelled = true
      if (unlisten) {
        unlisten()
      }
    }
  }, [])

  return event
}

function normalize(payload: unknown): PetOneShotKind | null {
  let candidate: unknown = payload
  if (
    candidate &&
    typeof candidate === "object" &&
    "payload" in (candidate as Record<string, unknown>)
  ) {
    candidate = (candidate as { payload: unknown }).payload
  }
  if (typeof candidate === "string" && ONESHOT_KINDS.has(candidate)) {
    return candidate as PetOneShotKind
  }
  return null
}
