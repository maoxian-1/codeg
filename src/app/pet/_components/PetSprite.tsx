"use client"

import { useMemo } from "react"
import {
  SPRITE_BACKGROUND_SIZE,
  backgroundPositionFor,
  type PetState,
} from "@/lib/pet/animation"
import { usePetAnimator } from "../_hooks/usePetAnimator"

export interface PetSpriteProps {
  spritesheetUrl: string
  state: PetState
  scale: number
  /** Aria-label for screen readers. */
  label: string
}

const FRAME_WIDTH = 192
const FRAME_HEIGHT = 208

export function PetSprite({
  spritesheetUrl,
  state,
  scale,
  label,
}: PetSpriteProps) {
  const { row, col } = usePetAnimator(state)
  const backgroundImage = useMemo(
    () => `url("${spritesheetUrl}")`,
    [spritesheetUrl]
  )

  return (
    <div
      role="img"
      aria-label={label}
      style={{
        width: `${FRAME_WIDTH * scale}px`,
        height: `${FRAME_HEIGHT * scale}px`,
        backgroundImage,
        backgroundRepeat: "no-repeat",
        backgroundSize: SPRITE_BACKGROUND_SIZE,
        backgroundPosition: backgroundPositionFor(row, col),
        imageRendering: "pixelated",
      }}
    />
  )
}
