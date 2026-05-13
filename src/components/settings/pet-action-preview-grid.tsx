"use client"

import { useEffect, useState, type CSSProperties } from "react"
import { useTranslations } from "next-intl"
import {
  PET_FRAME_DURATIONS_MS,
  PET_STATE_ROW,
  SPRITE_BACKGROUND_SIZE,
  backgroundPositionFor,
  type PetState,
} from "@/lib/pet/animation"

export const PET_ACTION_PREVIEW_STATES = [
  "idle",
  "running_right",
  "running_left",
  "waving",
  "jumping",
  "failed",
  "waiting",
  "running",
  "review",
] as const satisfies readonly PetState[]

export const MARKETPLACE_PREVIEW_FRAME_START = (() => {
  let start = 0
  const starts = {} as Record<PetState, number>
  for (const state of PET_ACTION_PREVIEW_STATES) {
    starts[state] = start
    start += PET_FRAME_DURATIONS_MS[state].length
  }
  return starts
})()

export const MARKETPLACE_PREVIEW_TOTAL_FRAMES =
  PET_ACTION_PREVIEW_STATES.reduce(
    (total, state) => total + PET_FRAME_DURATIONS_MS[state].length,
    0
  )

type PetActionPreviewSource =
  | {
      type: "marketplace"
      url: string
    }
  | {
      type: "spritesheet"
      url: string
    }

interface PetActionPreviewGridProps {
  petName: string
  source: PetActionPreviewSource
}

export function PetActionPreviewGrid({
  petName,
  source,
}: PetActionPreviewGridProps) {
  const t = useTranslations("Pet.marketplace")

  return (
    <div className="grid grid-cols-3 gap-1">
      {PET_ACTION_PREVIEW_STATES.map((state) => {
        const actionName = t(`actions.${state}`)
        return (
          <PetActionPreviewCell
            key={state}
            source={source}
            state={state}
            label={`${petName} ${actionName}`}
            actionName={actionName}
          />
        )
      })}
    </div>
  )
}

function PetActionPreviewCell({
  source,
  state,
  label,
  actionName,
}: {
  source: PetActionPreviewSource
  state: PetState
  label: string
  actionName: string
}) {
  const col = usePetActionPreviewFrame(state)
  const frameStyle =
    source.type === "marketplace"
      ? marketplacePreviewFrameStyle(source.url, state, col)
      : spritesheetPreviewFrameStyle(source.url, state, col)

  return (
    <div className="min-w-0 p-1">
      <div
        role="img"
        aria-label={label}
        className="rounded-sm bg-no-repeat"
        style={{
          aspectRatio: "12 / 13",
          imageRendering: "pixelated",
          ...frameStyle,
        }}
      />
      <div
        className="mt-0.5 truncate text-center text-[10px] leading-tight text-muted-foreground"
        title={actionName}
      >
        {actionName}
      </div>
    </div>
  )
}

function usePetActionPreviewFrame(state: PetState): number {
  const [col, setCol] = useState(0)

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null
    const durations = PET_FRAME_DURATIONS_MS[state]

    const playFrame = (nextCol: number) => {
      setCol(nextCol)
      const duration = durations[nextCol] ?? durations[durations.length - 1]
      timer = setTimeout(() => {
        playFrame((nextCol + 1) % durations.length)
      }, duration)
    }

    playFrame(0)

    return () => {
      if (timer) clearTimeout(timer)
    }
  }, [state])

  return col
}

function marketplacePreviewFrameStyle(
  previewUrl: string,
  state: PetState,
  col: number
): CSSProperties {
  const frame = MARKETPLACE_PREVIEW_FRAME_START[state] + col
  const x = (frame / (MARKETPLACE_PREVIEW_TOTAL_FRAMES - 1)) * 100
  return {
    backgroundImage: `url("${previewUrl}")`,
    backgroundSize: `${MARKETPLACE_PREVIEW_TOTAL_FRAMES * 100}% 100%`,
    backgroundPosition: `${x}% 0%`,
  }
}

function spritesheetPreviewFrameStyle(
  spritesheetUrl: string,
  state: PetState,
  col: number
): CSSProperties {
  return {
    backgroundImage: `url("${spritesheetUrl}")`,
    backgroundSize: SPRITE_BACKGROUND_SIZE,
    backgroundPosition: backgroundPositionFor(PET_STATE_ROW[state], col),
  }
}
