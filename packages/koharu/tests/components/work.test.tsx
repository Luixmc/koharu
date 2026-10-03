import { QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { WorkPage } from '@/components/work/WorkPage'
import { queryClient } from '@/lib/queries'
import { useWorkStore } from '@/lib/work'
import { commands, type Work } from '@koharu/bridge/protocol'

const work: Work = {
  ficha: '# Personajes\n\n- **Aki**: la hermana mayor.',
  notes: { tags: ['female:big breasts'], description: 'Dos hermanas.' },
  proposals: [{ source: 'お兄ちゃん', target: 'hermano', note: 'trato' }],
  terms: [],
  corrections: [
    {
      id: 'c1',
      page: '1',
      original: '好き',
      current: 'Te quiero mucho',
      proposal: 'Me gustas mucho',
      reason: 'registro',
      speaker: 'Aki → Yuu (duda: no se ve quién)',
    },
  ],
  approved_corrections: 0,
  balloons: [],
  next: { kind: 'proposals', count: 1 },
}

function renderWork() {
  vi.spyOn(commands, 'getProject').mockResolvedValue(null)
  vi.spyOn(commands, 'listProjects').mockResolvedValue([{ name: 'Sakurami JA' }])
  vi.spyOn(commands, 'getWork').mockResolvedValue(work)
  vi.spyOn(commands, 'getModelChoices').mockResolvedValue({
    study: 'cydonia',
    translation: 'gemma',
    deepl: false,
    review: 'cydonia',
  })
  vi.spyOn(commands, 'getLlmModels').mockResolvedValue(['cydonia', 'gemma'])
  vi.spyOn(commands, 'getRecommendations').mockResolvedValue({
    ocr: { ja: 'baberu-ocr', ko: 'hayai-ocr', zh: 'paddleocr-vl-1.6', en: 'paddleocr-vl-1.6' },
    models: [
      {
        id: 'cydonia',
        tasks: ['corregir'],
        gb: 11.9,
        languages: {},
        measured: { corregir: 'muy bueno' },
        note: '',
        installed: true,
      },
      {
        id: 'qwen3-14b',
        tasks: ['traducir'],
        gb: 9,
        languages: {},
        measured: {},
        note: 'SIN PROBAR.',
        installed: false,
      },
    ],
  })
  return render(
    <QueryClientProvider client={queryClient}>
      <WorkPage />
    </QueryClientProvider>,
  )
}

describe('WorkPage', () => {
  afterEach(() => {
    vi.restoreAllMocks()
    useWorkStore.setState({ project: null, tab: 'process', queue: null, log: [] })
  })

  it('queues the ticked steps with the language the project name gives', async () => {
    const enqueue = vi.spyOn(commands, 'enqueue').mockResolvedValue(null)
    renderWork()
    expect(await screen.findByText('khr reads this language with Baberu OCR.')).toBeInTheDocument()
    const run = screen.getByRole('button', { name: 'Run' })
    await waitFor(() => expect(run).toBeEnabled())
    fireEvent.click(run)
    await waitFor(() => expect(enqueue).toHaveBeenCalled())
    expect(enqueue.mock.calls[0][0]).toMatchObject({
      project: 'Sakurami JA',
      stages: ['detection', 'ocr', 'inpainting'],
      language: 'ja',
      notes: { tags: ['female:big breasts'], description: 'Dos hermanas.' },
    })
  })

  it('shows the ficha and approves a proposal as edited', async () => {
    const decide = vi.spyOn(commands, 'decideTerms').mockResolvedValue(null)
    renderWork()
    fireEvent.click(await screen.findByRole('button', { name: 'Ficha' }))
    expect(await screen.findByRole('heading', { name: 'Personajes' })).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: /Glossary/ }))
    const rendering = await screen.findByRole('textbox', { name: 'Rendering of お兄ちゃん' })
    fireEvent.change(rendering, { target: { value: 'hermanito' } })
    const row = rendering.closest('li')
    if (!row) throw new Error('proposal row is missing')
    fireEvent.click(within(row).getByRole('button', { name: 'Approve' }))
    await waitFor(() =>
      expect(decide).toHaveBeenCalledWith(
        'Sakurami JA',
        [{ source: 'お兄ちゃん', target: 'hermanito', note: 'trato' }],
        true,
      ),
    )
  })

  it('marks what a correction removes and adds', async () => {
    renderWork()
    fireEvent.click(await screen.findByRole('button', { name: /Corrections/ }))
    expect(await screen.findByText('Te quiero')).toHaveClass('line-through')
    expect(screen.getByText('Me gustas')).not.toHaveClass('line-through')
    expect(screen.getByTitle('The reviewer is unsure: no se ve quién')).toBeInTheDocument()
  })
})
