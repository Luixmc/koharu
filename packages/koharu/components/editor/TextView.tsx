'use client'

import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronLeft, ChevronRight, CircleHelp, Lightbulb } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { CommitTextarea } from '@/components/controls/CommitTextarea'
import { call } from '@/lib/backend'
import { pageKey, projectKey, refresh, usePage, usePages } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { showCanvasPage } from '@koharu/bridge/canvas'
import { commands, type Layer, type ProjectInfo, type ReviewNote } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { cn } from '@koharu/ui/lib/utils'

type TextLayer = Extract<Layer, { type: 'text' }>

export const reviewNotesKey = ['review-notes'] as const

/**
 * The page as text: every balloon's original beside its translation, both
 * editable, with what khr's reviewer flagged. It needs no image, so it is the
 * view to keep open while the images are retracted.
 */
export function TextView() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const page = usePage().data ?? null
  const pages = usePages().data ?? []
  const selectedLayers = useKoharuStore((state) => state.selectedLayers)
  const selectLayers = useKoharuStore((state) => state.selectLayers)
  const selectPages = useKoharuStore((state) => state.selectPages)
  const [onlyFlagged, setOnlyFlagged] = useState(false)

  const notes = useQuery({
    queryKey: reviewNotesKey,
    queryFn: () => call(commands.getReviewNotes),
    staleTime: 30_000,
  }).data
  const notesByContent = useMemo(
    () => new Map((notes ?? []).map((note) => [note.content, note])),
    [notes],
  )

  const balloons = (page?.layers ?? []).filter((layer): layer is TextLayer => layer.type === 'text')
  const flagged = (layer: TextLayer) => {
    const note = notesByContent.get(layer.content.id)
    return Boolean(note?.doubt || note?.proposal)
  }
  const shown = onlyFlagged ? balloons.filter(flagged) : balloons
  const flaggedCount = balloons.filter(flagged).length

  const index = page ? pages.findIndex((summary) => summary.id === page.id) : -1
  const goTo = (offset: number) => {
    const target = pages[index + offset]
    if (!target) return
    const project = queryClient.getQueryData<ProjectInfo | null>(projectKey)
    showCanvasPage(target.id, project?.revision ?? null)
    selectPages([target.id])
    selectLayers([])
    void call(commands.selectPage, target.id)
      .then((selection) => {
        queryClient.setQueryData(projectKey, selection.project)
        queryClient.setQueryData(pageKey, selection.page)
      })
      .catch(() => undefined)
  }

  if (!page) {
    return (
      <div className='grid h-full place-items-center text-[12px] text-muted-foreground'>
        {t('textView.noPage')}
      </div>
    )
  }

  return (
    <div className='flex h-full min-h-0 flex-col bg-[var(--surface-canvas)]'>
      <div className='flex h-10 shrink-0 items-center gap-2 border-b border-border/40 px-3'>
        <Button
          variant='ghost'
          size='icon-xs'
          aria-label={t('textView.previous')}
          disabled={index <= 0}
          onClick={() => goTo(-1)}
        >
          <ChevronLeft />
        </Button>
        <span className='text-[12px] font-medium tabular-nums'>
          {t('textView.page', { page: index + 1, total: pages.length })}
        </span>
        <Button
          variant='ghost'
          size='icon-xs'
          aria-label={t('textView.next')}
          disabled={index < 0 || index >= pages.length - 1}
          onClick={() => goTo(1)}
        >
          <ChevronRight />
        </Button>
        <div className='flex-1' />
        <label className='flex items-center gap-1.5 text-[11px] text-muted-foreground'>
          <input
            type='checkbox'
            checked={onlyFlagged}
            onChange={(event) => setOnlyFlagged(event.currentTarget.checked)}
          />
          {t('textView.onlyFlagged', { count: flaggedCount })}
        </label>
      </div>
      <div className='grid shrink-0 grid-cols-2 gap-3 px-4 pt-2 text-[9px] font-medium tracking-[0.06em] text-muted-foreground uppercase'>
        <span>{t('inspector.source')}</span>
        <span>{t('inspector.translation')}</span>
      </div>
      <ScrollArea className='min-h-0 flex-1'>
        <ol className='grid gap-2 p-3'>
          {shown.length === 0 && (
            <li className='py-8 text-center text-[12px] text-muted-foreground'>
              {onlyFlagged ? t('textView.nothingFlagged') : t('textView.noText')}
            </li>
          )}
          {shown.map((layer) => (
            <Balloon
              key={layer.id}
              layer={layer}
              number={balloons.indexOf(layer) + 1}
              note={notesByContent.get(layer.content.id)}
              selected={selectedLayers.includes(layer.id)}
              onSelect={() => selectLayers([layer.id])}
            />
          ))}
        </ol>
      </ScrollArea>
    </div>
  )
}

function Balloon({
  layer,
  number,
  note,
  selected,
  onSelect,
}: {
  layer: TextLayer
  number: number
  note: ReviewNote | undefined
  selected: boolean
  onSelect: () => void
}) {
  const { t } = useTranslation()
  const translation = layer.content.translation?.text ?? ''

  return (
    <li
      onFocusCapture={onSelect}
      className={cn(
        'grid gap-1.5 rounded-xl border p-2',
        selected ? 'border-primary/40 bg-primary/[0.05]' : 'border-border/40',
        (note?.doubt || note?.proposal) && !selected && 'border-amber-500/40',
      )}
    >
      <div className='flex items-center gap-2 text-[10px] text-muted-foreground'>
        <span className='font-medium tabular-nums'>#{number}</span>
        {note?.speaker && <span className='truncate'>{note.speaker}</span>}
      </div>
      <div className='grid grid-cols-2 gap-3'>
        <CommitTextarea
          aria-label={t('textView.sourceOf', { number })}
          className='min-h-12 resize-y text-[12px] leading-4 md:text-[12px]'
          value={layer.content.source?.text ?? ''}
          onCommit={(text) =>
            void call(commands.setSourceText, layer.id, text)
              .then(() => refresh(projectKey, pageKey))
              .catch(() => undefined)
          }
        />
        <CommitTextarea
          aria-label={t('textView.translationOf', { number })}
          className='min-h-12 resize-y border-primary/25 text-[12px] leading-4 md:text-[12px]'
          value={translation}
          onCommit={(text) =>
            void call(commands.setTranslation, layer.id, text.trim() ? text : null)
              .then(() => refresh(projectKey, pageKey))
              .catch(() => undefined)
          }
        />
      </div>
      {note?.doubt && (
        <p className='flex items-start gap-1.5 text-[11px] text-amber-600 dark:text-amber-400'>
          <CircleHelp className='mt-0.5 size-3 shrink-0' />
          <span>{t('textView.doubt', { doubt: note.doubt })}</span>
        </p>
      )}
      {note?.proposal && note.proposal !== translation && (
        <div className='flex items-start gap-1.5 text-[11px]'>
          <Lightbulb className='mt-0.5 size-3 shrink-0 text-amber-600 dark:text-amber-400' />
          <span className='min-w-0 flex-1'>
            {t('textView.proposal', { proposal: note.proposal })}
            {note.reason && <span className='text-muted-foreground'> — {note.reason}</span>}
          </span>
          <Button
            variant='outline'
            size='xs'
            onClick={() =>
              void call(commands.setTranslation, layer.id, note.proposal)
                .then(() => refresh(projectKey, pageKey))
                .catch(() => undefined)
            }
          >
            {t('textView.useProposal')}
          </Button>
        </div>
      )}
    </li>
  )
}
