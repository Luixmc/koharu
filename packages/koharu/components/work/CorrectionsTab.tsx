'use client'

import { BookPlus, Check, FileSearch, Wand2, X } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { wordDiff, type Piece } from '@/lib/diff'
import { refresh } from '@/lib/queries'
import { useWork, useWorkStore, workKey } from '@/lib/work'
import { commands, type Correction } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'
import { Textarea } from '@koharu/ui/components/textarea'
import { cn } from '@koharu/ui/lib/utils'

/**
 * The reviewer's proposals inside their pages, in reading order: what goes
 * struck out, what comes in, editable before approving. Approved text
 * reaches the project when the approved list is applied.
 */
export function CorrectionsTab({ project }: { project: string }) {
  const { t } = useTranslation()
  const work = useWork(project).data
  const leftToRight = useWorkStore((state) => state.leftToRight)
  const [corrections, setCorrections] = useState<Correction[]>([])
  const [newTerm, setNewTerm] = useState<{ source: string; target: string } | null>(null)
  useEffect(() => setCorrections(work?.corrections ?? []), [work?.corrections])
  useEffect(() => setNewTerm(null), [project])

  const pages = useMemo(() => [...new Set(corrections.map(({ page }) => page))], [corrections])
  const balloons = work?.balloons ?? []

  const decide = async (decided: Correction[], approve: boolean) => {
    await call(commands.decideCorrections, project, decided, approve)
    await refresh(workKey(project))
  }
  const edit = (id: string, proposal: string) =>
    setCorrections((current) =>
      current.map((item) => (item.id === id ? { ...item, proposal } : item)),
    )
  const toGlossary = (correction: Correction) => {
    setNewTerm({ source: correction.original, target: correction.proposal })
    void decide([correction], true).catch(() => undefined)
  }
  const saveTerm = async () => {
    if (!newTerm) return
    await call(commands.addTerm, project, { ...newTerm, note: t('work.corrections.termNote') })
    setNewTerm(null)
    await refresh(workKey(project))
  }

  const card = (correction: Correction) => (
    <CorrectionCard
      key={correction.id}
      correction={correction}
      onEdit={(proposal) => edit(correction.id, proposal)}
      onApprove={() => void decide([correction], true).catch(() => undefined)}
      onReject={() => void decide([correction], false).catch(() => undefined)}
      onGlossary={() => toGlossary(correction)}
    />
  )

  return (
    <div className='grid gap-6'>
      <div className='flex flex-wrap items-center gap-2 text-[11px]'>
        <span className='text-muted-foreground'>
          {t('work.corrections.summary', {
            pending: corrections.length,
            approved: work?.approved_corrections ?? 0,
          })}
        </span>
        <Button
          type='button'
          size='sm'
          className='ml-auto h-8 gap-1.5 text-[11px]'
          disabled={!work?.approved_corrections}
          title={t('work.corrections.applyHint')}
          onClick={() => void call(commands.applyCorrections, project).catch(() => undefined)}
        >
          <Wand2 className='size-3.5' /> {t('work.corrections.apply')}
        </Button>
      </div>

      {newTerm && (
        <section className='grid gap-2 rounded-xl border border-primary/40 bg-primary/5 p-4 text-[11px]'>
          <p>{t('work.corrections.newTermHint')}</p>
          <div className='grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto_auto] items-center gap-2'>
            <Input
              value={newTerm.source}
              aria-label={t('work.corrections.termSource')}
              placeholder={t('work.corrections.termSource')}
              onChange={(event) => setNewTerm({ ...newTerm, source: event.target.value })}
              className='h-8 text-[11px]'
            />
            <Input
              value={newTerm.target}
              aria-label={t('work.corrections.termTarget')}
              placeholder={t('work.corrections.termTarget')}
              onChange={(event) => setNewTerm({ ...newTerm, target: event.target.value })}
              className='h-8 text-[11px]'
            />
            <Button
              type='button'
              size='sm'
              className='h-8 text-[11px]'
              onClick={() => void saveTerm().catch(() => undefined)}
            >
              {t('work.corrections.saveTerm')}
            </Button>
            <Button
              type='button'
              variant='ghost'
              size='sm'
              className='h-8 text-[11px]'
              onClick={() => setNewTerm(null)}
            >
              {t('common.cancel')}
            </Button>
          </div>
        </section>
      )}

      {corrections.length === 0 ? (
        <p className='text-[11px] text-muted-foreground'>{t('work.corrections.empty')}</p>
      ) : (
        <>
          <div className='flex flex-wrap items-center gap-2 text-[11px]'>
            <span className='text-muted-foreground'>
              {t('work.corrections.legendBefore')}{' '}
              <DiffText
                pieces={[['removed', t('work.corrections.legendRemoved')]]}
                side='removed'
              />{' '}
              <DiffText pieces={[['added', t('work.corrections.legendAdded')]]} side='added' />
            </span>
            {balloons.length === 0 && (
              <Button
                type='button'
                variant='outline'
                size='sm'
                className='h-7 gap-1.5 text-[11px]'
                title={t('work.corrections.contextHint')}
                onClick={() =>
                  void call(commands.readPageContext, project, leftToRight).catch(() => undefined)
                }
              >
                <FileSearch className='size-3.5' /> {t('work.corrections.context')}
              </Button>
            )}
            <Button
              type='button'
              size='sm'
              className='ml-auto h-7 text-[11px]'
              onClick={() => void decide(corrections, true).catch(() => undefined)}
            >
              {t('work.approveAll')}
            </Button>
            <Button
              type='button'
              variant='outline'
              size='sm'
              className='h-7 text-[11px]'
              onClick={() => void decide(corrections, false).catch(() => undefined)}
            >
              {t('work.rejectAll')}
            </Button>
          </div>

          {pages.map((page) => {
            const inPage = balloons.filter((balloon) => balloon.page === page)
            const shown = new Set<string>()
            return (
              <section key={page} className='grid gap-2'>
                <h2 className='text-[13px] font-semibold'>
                  {t('work.corrections.page', { page })}
                </h2>
                {inPage.map((balloon) => {
                  const correction = corrections.find(({ id }) => id === balloon.id)
                  if (correction) {
                    shown.add(correction.id)
                    return card(correction)
                  }
                  return (
                    <p
                      key={balloon.id}
                      className={cn('text-[12px]', !balloon.translation && 'text-muted-foreground')}
                      title={
                        balloon.translation ? balloon.original : t('work.corrections.untranslated')
                      }
                    >
                      <Speaker speaker={balloon.speaker} />
                      {balloon.translation || `(${balloon.original})`}
                    </p>
                  )
                })}
                {/* Proposals whose balloon the saved page does not hold (no
                    context yet, or the page changed). */}
                {corrections
                  .filter((correction) => correction.page === page && !shown.has(correction.id))
                  .map(card)}
              </section>
            )
          })}
        </>
      )}
    </div>
  )
}

function CorrectionCard({
  correction,
  onEdit,
  onApprove,
  onReject,
  onGlossary,
}: {
  correction: Correction
  onEdit: (proposal: string) => void
  onApprove: () => void
  onReject: () => void
  onGlossary: () => void
}) {
  const { t } = useTranslation()
  const pieces = wordDiff(correction.current, correction.proposal)
  return (
    <div className='grid gap-2 rounded-xl border-[1.5px] border-amber-500/70 bg-[var(--surface-panel)] p-3 text-[12px]'>
      <p className='text-muted-foreground'>
        <Speaker speaker={correction.speaker} />
        {correction.original}
      </p>
      <p>
        <span className='mr-2 font-semibold'>{t('work.corrections.now')}</span>
        <DiffText pieces={pieces} side='removed' />
      </p>
      <p>
        <span className='mr-2 font-semibold'>{t('work.corrections.becomes')}</span>
        <DiffText pieces={pieces} side='added' />
      </p>
      <Textarea
        value={correction.proposal}
        rows={1}
        aria-label={t('work.corrections.proposal')}
        onChange={(event) => onEdit(event.target.value)}
        className='min-h-8 text-[12px]'
      />
      {correction.reason && (
        <p className='text-[11px] text-muted-foreground italic'>{correction.reason}</p>
      )}
      <div className='flex gap-2'>
        <Button type='button' size='sm' className='h-7 gap-1 text-[11px]' onClick={onApprove}>
          <Check className='size-3.5' /> {t('work.approve')}
        </Button>
        <Button
          type='button'
          variant='outline'
          size='sm'
          className='h-7 gap-1 text-[11px]'
          onClick={onReject}
        >
          <X className='size-3.5' /> {t('work.reject')}
        </Button>
        <Button
          type='button'
          variant='ghost'
          size='sm'
          className='h-7 gap-1 text-[11px]'
          onClick={onGlossary}
        >
          <BookPlus className='size-3.5' /> {t('work.corrections.approveAndTerm')}
        </Button>
      </div>
    </div>
  )
}

/** One side of a difference: the shared words plus that side's, marked. */
function DiffText({ pieces, side }: { pieces: [Piece, string][]; side: Exclude<Piece, 'same'> }) {
  return (
    <span>
      {pieces
        .filter(([piece]) => piece === 'same' || piece === side)
        .map(([piece, text], index) => (
          <span key={index}>
            {index > 0 && ' '}
            <span
              className={cn(
                piece === 'removed' &&
                  'rounded-sm bg-red-200 text-red-800 line-through dark:bg-red-950 dark:text-red-300',
                piece === 'added' &&
                  'rounded-sm bg-green-200 text-green-800 dark:bg-green-950 dark:text-green-300',
              )}
            >
              {text}
            </span>
          </span>
        ))}
    </span>
  )
}

/**
 * Who speaks a balloon, as the reviewer read it; amber when the reviewer was
 * unsure, so a proposal built on it is read with care.
 */
function Speaker({ speaker }: { speaker: string }) {
  const { t } = useTranslation()
  if (!speaker) return null
  const at = speaker.indexOf(' (duda: ')
  const name = at < 0 ? speaker : speaker.slice(0, at)
  const doubt = at < 0 ? null : speaker.slice(at + ' (duda: '.length).replace(/\)$/, '')
  return (
    <span
      className={cn(
        'mr-1.5 text-[10px]',
        doubt ? 'text-amber-600 dark:text-amber-400' : 'text-muted-foreground',
      )}
      title={doubt ? t('work.corrections.doubt', { doubt }) : t('work.corrections.speaker')}
    >
      [{name}]
    </span>
  )
}
