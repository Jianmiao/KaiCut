import { useEffect } from 'react'
import { invoke, isTauri } from '@tauri-apps/api/core'
import { usePreferences } from '@/stores/preferences-store'

interface NativeAppearance { material: 'native' | 'solid'; message: string }

export function useApplyPreferences() {
  const { transparency, reducedMotion } = usePreferences()
  useEffect(() => {
    const motion = matchMedia('(prefers-reduced-motion: reduce)')
    const reduceTransparency = matchMedia('(prefers-reduced-transparency: reduce)')
    const contrast = matchMedia('(forced-colors: active)')
    let active = true
    let revision = 0
    const apply = () => {
      const root = document.documentElement
      const resolved = 'dark'
      const allowTransparency = transparency && !reduceTransparency.matches && !contrast.matches
      const currentRevision = ++revision
      root.dataset.theme = resolved
      root.classList.toggle('dark', resolved === 'dark')
      root.dataset.transparency = allowTransparency ? 'on' : 'off'
      root.dataset.motion = reducedMotion || motion.matches ? 'reduce' : 'full'
      root.dataset.platform = /Mac/.test(navigator.platform) ? 'macos' : /Win/.test(navigator.platform) ? 'windows' : 'other'
      if (!allowTransparency || !isTauri()) root.dataset.nativeMaterial = 'solid'
      if (isTauri()) {
        void invoke<NativeAppearance>('apply_window_appearance', { theme: resolved, transparency: allowTransparency })
          .then((result) => {
            if (!active || currentRevision !== revision) return
            root.dataset.nativeMaterial = result.material
            if (result.material === 'solid') root.dataset.transparency = 'off'
            root.dataset.materialReason = result.message
          })
          .catch(() => {
            if (!active || currentRevision !== revision) return
            root.dataset.nativeMaterial = 'solid'
            root.dataset.transparency = 'off'
            root.dataset.materialReason = '原生材质不可用，使用实色'
          })
      }
    }
    apply()
    const queries = [motion, reduceTransparency, contrast]
    for (const query of queries) query.addEventListener('change', apply)
    window.addEventListener('focus', apply)
    return () => {
      active = false
      for (const query of queries) query.removeEventListener('change', apply)
      window.removeEventListener('focus', apply)
    }
  }, [transparency, reducedMotion])
}
