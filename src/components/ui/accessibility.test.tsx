import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it } from 'vitest'
import { Dialog } from './dialog'
import { Tabs, TabsList, TabsTrigger, TabsContent } from './tabs'

let host: HTMLDivElement
let root: Root
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  host = document.createElement('div')
  host.id = 'root'
  document.body.appendChild(host)
  root = createRoot(host)
})
afterEach(async () => { await act(() => root.unmount()); host.remove() })

it('traps dialog focus and preserves contents when confirmation returns false', async () => {
  await act(() => root.render(<Dialog open onOpenChange={() => {}} title="检查" onConfirm={() => false}><input aria-label="名称" /></Dialog>))
  const dialog = document.querySelector('[role="dialog"]')!
  expect(dialog.getAttribute('aria-modal')).toBe('true')
  expect(dialog.contains(document.activeElement)).toBe(true)
  const buttons = dialog.querySelectorAll('button')
  const last = buttons[buttons.length - 1]
  last.focus()
  await act(() => last.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true })))
  expect(dialog.contains(document.activeElement)).toBe(true)
  await act(() => last.click())
  expect(document.querySelector('[role="dialog"]')).not.toBeNull()
})

it('links tabs to panels and supports keyboard activation', async () => {
  await act(() => root.render(<Tabs defaultValue="list"><TabsList aria-label="视图"><TabsTrigger value="list">列表</TabsTrigger><TabsTrigger value="grid">网格</TabsTrigger></TabsList><TabsContent value="list">列表内容</TabsContent><TabsContent value="grid">网格内容</TabsContent></Tabs>))
  const tabs = host.querySelectorAll<HTMLButtonElement>('[role="tab"]')
  tabs[0].focus()
  await act(() => tabs[0].dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true })))
  expect(tabs[1].getAttribute('aria-selected')).toBe('true')
  const panel = document.getElementById(tabs[1].getAttribute('aria-controls')!)!
  expect(panel.hidden).toBe(false)
  expect(panel.getAttribute('aria-labelledby')).toBe(tabs[1].id)
})
