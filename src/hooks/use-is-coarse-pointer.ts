"use client"

import { useEffect, useState } from "react"

export function useIsCoarsePointer() {
  const [isCoarsePointer, setIsCoarsePointer] = useState(false)

  useEffect(() => {
    const query = window.matchMedia("(pointer: coarse)")
    const update = () => setIsCoarsePointer(query.matches)

    update()
    query.addEventListener("change", update)
    return () => query.removeEventListener("change", update)
  }, [])

  return isCoarsePointer
}
