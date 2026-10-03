import { describe, expect, it } from 'vitest'

import { wordDiff } from '@/lib/diff'

describe('word diff', () => {
  it('marks only the changed words', () => {
    expect(wordDiff('Este primavera me caso', 'Esta primavera me caso contigo')).toEqual([
      ['removed', 'Este'],
      ['added', 'Esta'],
      ['same', 'primavera me caso'],
      ['added', 'contigo'],
    ])
  })
})
