"use client"

import { useEffect, useRef } from "react"
import { useTranslations } from "next-intl"
import {
  closePetWindow,
  savePetWindowState,
  showPetContextMenu,
} from "@/lib/pet/api"
import { disposeTauriListener } from "@/lib/tauri-listener"
import { isDesktop } from "@/lib/transport"

export interface PetMenuProps {
  onScaleChange: (scale: number) => void
  onOpenSettings: () => void
}

type MenuAction =
  | { type: "scale"; value: number }
  | { type: "open_manager" }
  | { type: "close" }

/**
 * Right-click controller. Renders no UI of its own — the menu itself is
 * native (built in Rust via `pet_show_context_menu` and dispatched through
 * Tauri's menu-event bus). HTML popups got clipped to the tiny pet window
 * and were unclosable when they overflowed; the OS-level menu sidesteps
 * both problems and gives us free Escape/click-outside dismiss.
 */
export function PetMenu({ onScaleChange, onOpenSettings }: PetMenuProps) {
  const t = useTranslations("Pet")

  // Stash the latest callbacks in a ref so the menu-action listener (set up
  // once at mount) always calls into the current closures without having
  // to re-subscribe on every render. PetWindow rerenders on each animation
  // tick and recreates `openManager` inline, so a deps-based effect would
  // tear down and rebuild the Tauri listener constantly — a window during
  // which a menu-action event would be silently dropped.
  const callbacksRef = useRef({ onScaleChange, onOpenSettings })
  useEffect(() => {
    callbacksRef.current = { onScaleChange, onOpenSettings }
  }, [onScaleChange, onOpenSettings])

  // Stash translations the same way: the right-click listener pulls fresh
  // labels at popup time without needing to rebind.
  const tRef = useRef(t)
  useEffect(() => {
    tRef.current = t
  }, [t])

  // 1) Hook the right-click and forward to the native menu.
  useEffect(() => {
    if (!isDesktop()) return
    function onContextMenu(e: MouseEvent) {
      e.preventDefault()
      const x = e.clientX
      const y = e.clientY
      const tNow = tRef.current
      void showPetContextMenu(
        {
          scale: tNow("menu.scale"),
          openManager: tNow("menu.openManager"),
          close: tNow("menu.close"),
        },
        x,
        y
      ).catch((err) => {
        console.warn("[Pet] failed to show context menu:", err)
      })
    }
    document.addEventListener("contextmenu", onContextMenu)
    return () => {
      document.removeEventListener("contextmenu", onContextMenu)
    }
  }, [])

  // 2) Listen for actions emitted by the native menu's event handler.
  //    Mount-once subscription — see callbacksRef rationale above.
  useEffect(() => {
    if (!isDesktop()) return
    let unlisten: (() => void) | null = null
    let cancelled = false
    void (async () => {
      try {
        const { listen } = await import("@tauri-apps/api/event")
        const off = await listen<MenuAction>("pet://menu-action", (event) => {
          const action = event.payload
          if (!action) return
          const { onScaleChange, onOpenSettings } = callbacksRef.current
          if (action.type === "scale") {
            const next = action.value
            void savePetWindowState({ scale: next })
              .then(() => onScaleChange(next))
              .catch((err) => {
                console.warn("[Pet] scale change failed:", err)
              })
          } else if (action.type === "open_manager") {
            onOpenSettings()
          } else if (action.type === "close") {
            void closePetWindow().catch((err) => {
              console.warn("[Pet] close failed:", err)
            })
          }
        })
        if (cancelled) {
          disposeTauriListener(off, "Pet")
        } else {
          unlisten = off
        }
      } catch (err) {
        console.warn("[Pet] menu-action subscription failed:", err)
      }
    })()
    return () => {
      cancelled = true
      disposeTauriListener(unlisten, "Pet")
    }
  }, [])

  return null
}
