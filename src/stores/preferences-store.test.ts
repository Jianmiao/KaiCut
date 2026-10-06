import { beforeEach, expect, it } from 'vitest'
import { usePreferences } from './preferences-store'

beforeEach(() => {
  localStorage.clear()
  usePreferences.setState({ transparency: true, reducedMotion: false })
})

it('drops a saved light theme without losing accessibility preferences', async () => {
  localStorage.setItem('kaicut-preferences-v1', JSON.stringify({
    state: { theme: 'light', transparency: false, reducedMotion: true }, version: 0,
  }))
  await usePreferences.persist.rehydrate()
  expect(usePreferences.getState()).not.toHaveProperty('theme')
  expect(usePreferences.getState().transparency).toBe(false)
  expect(usePreferences.getState().reducedMotion).toBe(true)
})
