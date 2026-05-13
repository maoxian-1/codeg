"use client"

import { useEffect, useState } from "react"
import { getCurrentPetState } from "@/lib/pet/api"
import { isDesktop } from "@/lib/transport"
import type { PetState } from "@/lib/pet/animation"

const PET_STATE_EVENT = "pet://state"

export function usePetState(initial: PetState = "idle"): PetState {
  const [state, setState] = useState<PetState>(initial)

  useEffect(() => {
    let unlisten: (() => void) | null = null
    let cancelled = false
    // The mapper only emits `pet://state` on changes, so a window opened
    // mid-conversation would never see the current Running/Review state.
    // The Tauri event plugin in particular doesn't buffer events emitted
    // during the `await invoke("plugin:event|listen")` gap, so the snapshot
    // fetch below is the only reliable way to recover the missed state.
    // A live event may also arrive between subscribe-armed and snapshot-
    // resolved — once it does, the snapshot is stale and must not overwrite
    // it (the live event is more recent).
    let liveEventSeen = false
    const applyLive = (next: PetState | null) => {
      if (cancelled || !next) return
      liveEventSeen = true
      setState(next)
    }

    async function subscribe() {
      try {
        if (isDesktop()) {
          const { listen } = await import("@tauri-apps/api/event")
          const off = await listen<PetState>(PET_STATE_EVENT, (event) => {
            applyLive(normalize(event.payload))
          })
          if (cancelled) {
            off()
            return
          }
          unlisten = off
        } else {
          const { getTransport } = await import("@/lib/transport")
          const off = await getTransport().subscribe<PetState>(
            PET_STATE_EVENT,
            (payload) => {
              applyLive(normalize(payload))
            }
          )
          if (cancelled) {
            off()
            return
          }
          unlisten = off
        }

        // Subscription is armed — pull the current snapshot to recover the
        // state for windows that mounted after the last transition. If a
        // live event raced in first, leave its value in place. We thread
        // the snapshot through the same `normalize()` as the live path so
        // callers can't accidentally feed `setState` a non-PetState value.
        try {
          const snapshot = normalize(await getCurrentPetState())
          if (cancelled || liveEventSeen || !snapshot) return
          setState(snapshot)
        } catch (err) {
          console.warn("[Pet] state snapshot fetch failed:", err)
        }
      } catch (err) {
        // Subscription failures are non-fatal — pet just stays in `idle`.
        console.warn("[Pet] state subscription failed:", err)
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

  return state
}

function normalize(payload: unknown): PetState | null {
  if (typeof payload === "string") {
    return payload as PetState
  }
  if (
    payload &&
    typeof payload === "object" &&
    "payload" in (payload as Record<string, unknown>)
  ) {
    const inner = (payload as { payload: unknown }).payload
    if (typeof inner === "string") return inner as PetState
  }
  return null
}
