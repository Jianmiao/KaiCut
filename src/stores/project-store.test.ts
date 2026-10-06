import { beforeEach, describe, expect, it, vi } from 'vitest'
vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: vi.fn() }))
import { invoke } from '@tauri-apps/api/core'
import { useProjectStore } from './project-store'
import { createEmptyProject } from '@/lib/kaicut-format'

describe('native project projection', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset()
    useProjectStore.setState({ project: createEmptyProject('仍需保存'), media: [], isDirty: true, projectPath: null, busy: false, statusMessage: '' })
  })
  it('keeps dirty state and contents after a failed save', async () => {
    vi.mocked(invoke).mockRejectedValue('磁盘空间不足')
    await useProjectStore.getState().saveProject()
    expect(useProjectStore.getState().isDirty).toBe(true)
    expect(useProjectStore.getState().project.name).toBe('仍需保存')
    expect(useProjectStore.getState().statusMessage).toContain('磁盘空间不足')
    expect(useProjectStore.getState().busy).toBe(false)
  })
  it('does not clear dirty state when a native picker is cancelled', async () => {
    vi.mocked(invoke).mockResolvedValue({ project: useProjectStore.getState().project, path: null, dirty: true, status: '已取消保存' })
    await useProjectStore.getState().saveProject()
    expect(useProjectStore.getState().isDirty).toBe(true)
  })
  it('accepts only the successful backend session as saved', async () => {
    vi.mocked(invoke).mockResolvedValue({ project: useProjectStore.getState().project, path: 'E:/我的项目.kaicut', dirty: false, status: '项目已保存' })
    await useProjectStore.getState().saveProject()
    expect(invoke).toHaveBeenCalledWith('project_save', undefined)
    expect(useProjectStore.getState().projectPath).toBe('E:/我的项目.kaicut')
    expect(useProjectStore.getState().isDirty).toBe(false)
  })
  it('preserves current project when open rejects invalid JSON', async () => {
    vi.mocked(invoke).mockRejectedValue('项目结构无效')
    await useProjectStore.getState().openProject()
    expect(useProjectStore.getState().project.name).toBe('仍需保存')
    expect(useProjectStore.getState().isDirty).toBe(true)
  })
})
