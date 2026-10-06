import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import { usePreferences } from '@/stores/preferences-store'
import { useApplyPreferences } from './use-preferences'

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: vi.fn() }))
let root: Root
let host: HTMLDivElement
function PreferencesFixture() { useApplyPreferences(); return null }

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  vi.mocked(invoke).mockReset()
  vi.stubGlobal('matchMedia', vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })))
  usePreferences.setState({ transparency: true, reducedMotion: false })
  document.documentElement.dataset.nativeMaterial = 'solid'
  host = document.createElement('div')
  document.body.appendChild(host)
  root = createRoot(host)
})
afterEach(async () => {
  await act(() => root.unmount())
  host.remove()
  vi.unstubAllGlobals()
})

it('reveals the native material only after the native API acknowledges it', async () => {
  let resolve!: (value: unknown) => void
  vi.mocked(invoke).mockImplementation(() => new Promise((done) => { resolve = done }))
  await act(() => root.render(<PreferencesFixture />))
  expect(document.documentElement.dataset.nativeMaterial).toBe('solid')
  await act(async () => { resolve({ material: 'native', message: 'Acrylic accepted' }) })
  expect(document.documentElement.dataset.nativeMaterial).toBe('native')
  expect(document.documentElement.dataset.theme).toBe('dark')
  expect(invoke).toHaveBeenCalledWith('apply_window_appearance', { theme: 'dark', transparency: true })
})

it('uses opaque surfaces when the native API fails', async () => {
  vi.mocked(invoke).mockRejectedValue(new Error('unsupported'))
  await act(() => root.render(<PreferencesFixture />))
  expect(document.documentElement.dataset.nativeMaterial).toBe('solid')
})

it('ignores a stale enable response after transparency has been disabled', async () => {
  const responses: Array<(value: unknown) => void> = []
  vi.mocked(invoke).mockImplementation(() => new Promise((done) => { responses.push(done) }))
  await act(() => root.render(<PreferencesFixture />))
  await act(() => usePreferences.getState().setTransparency(false))
  expect(document.documentElement.dataset.transparency).toBe('off')
  await act(async () => { responses[0]({ material: 'native', message: 'stale' }) })
  expect(document.documentElement.dataset.nativeMaterial).toBe('solid')
  await act(async () => { responses[1]({ material: 'solid', message: 'disabled' }) })
  expect(document.documentElement.dataset.nativeMaterial).toBe('solid')
})
