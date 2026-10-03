import { describe, expect, it } from 'vitest'

import { withInstalledModels } from '@/lib/work'
import type { CatalogModel } from '@koharu/bridge/protocol'

const saved = { study: 'cydonia', translation: 'gemma', deepl: false, review: 'cydonia' }

const model = (id: string, measured: Record<string, string>): CatalogModel => ({
  id,
  tasks: Object.keys(measured),
  gb: 8,
  languages: {},
  measured,
  note: '',
  installed: true,
})

describe('model choices', () => {
  it('use an installed model for every step', () => {
    expect(withInstalledModels(saved, ['gemma'], [])).toEqual({
      ...saved,
      study: 'gemma',
      review: 'gemma',
    })
  })

  it('prefer an installed model measured for the step', () => {
    const catalog = [model('qwen', { corregir: '8/10' })]
    expect(withInstalledModels(saved, ['gemma', 'qwen'], catalog)).toEqual({
      ...saved,
      study: 'qwen',
      review: 'qwen',
    })
  })

  it('stay as saved when nothing is installed', () => {
    expect(withInstalledModels(saved, [], [])).toBe(saved)
  })
})
