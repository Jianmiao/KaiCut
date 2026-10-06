import { create } from 'zustand'
import { persist } from 'zustand/middleware'

interface Preferences {
  transparency: boolean
  reducedMotion: boolean
  setTransparency: (value: boolean) => void
  setReducedMotion: (value: boolean) => void
}

export const usePreferences = create<Preferences>()(persist((set) => ({
  transparency: true, reducedMotion: false,
  setTransparency: (transparency) => set({ transparency }),
  setReducedMotion: (reducedMotion) => set({ reducedMotion }),
}), {
  name: 'kaicut-preferences-v1',
  partialize: ({ transparency, reducedMotion }) => ({ transparency, reducedMotion }),
  // Keep accessibility choices; discard legacy light/system theme preferences.
  merge: (persisted, current) => {
    const saved = persisted && typeof persisted === 'object' ? persisted as Record<string, unknown> : {}
    return {
      ...current,
      transparency: typeof saved.transparency === 'boolean' ? saved.transparency : current.transparency,
      reducedMotion: typeof saved.reducedMotion === 'boolean' ? saved.reducedMotion : current.reducedMotion,
    }
  },
}))
