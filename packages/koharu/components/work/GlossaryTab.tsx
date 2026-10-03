'use client'

import { ArrowUpRight, Check, ExternalLink, X } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { refresh } from '@/lib/queries'
import { useWork, workKey } from '@/lib/work'
import { commands, type Term } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'

/**
 * The terms the study or the learning proposed, to approve (the rendering may
 * be corrected first) or reject, and the work's approved terms.
 */
export function GlossaryTab({ project }: { project: string }) {
  const { t } = useTranslation()
  const work = useWork(project).data
  const [proposals, setProposals] = useState<Term[]>([])
  useEffect(() => setProposals(work?.proposals ?? []), [work?.proposals])

  const decide = async (terms: Term[], approve: boolean) => {
    await call(commands.decideTerms, project, terms, approve)
    await refresh(workKey(project))
  }
  const promote = (term: Term) => void call(commands.promoteTerm, term).catch(() => undefined)
  const open = (file: 'glossary' | 'global_glossary') =>
    void call(commands.openWorkFile, project, file).catch(() => undefined)

  return (
    <div className='grid gap-8'>
      <div className='flex flex-wrap items-center gap-2 text-[11px]'>
        <span className='text-muted-foreground'>
          {t('work.glossary.summary', {
            approved: work?.terms.length ?? 0,
            pending: proposals.length,
          })}
        </span>
        <Button
          type='button'
          variant='ghost'
          size='sm'
          className='ml-auto h-8 gap-1.5 text-[11px]'
          onClick={() => open('glossary')}
        >
          <ExternalLink className='size-3.5' /> {t('work.glossary.openWork')}
        </Button>
        <Button
          type='button'
          variant='ghost'
          size='sm'
          className='h-8 gap-1.5 text-[11px]'
          onClick={() => open('global_glossary')}
        >
          <ExternalLink className='size-3.5' /> {t('work.glossary.openGlobal')}
        </Button>
      </div>

      <section className='grid gap-3'>
        <div className='flex items-center gap-2'>
          <h2 className='text-[12px] font-semibold'>{t('work.glossary.proposals')}</h2>
          {proposals.length > 0 && (
            <>
              <Button
                type='button'
                size='sm'
                className='ml-auto h-7 text-[11px]'
                onClick={() => void decide(proposals, true).catch(() => undefined)}
              >
                {t('work.approveAll')}
              </Button>
              <Button
                type='button'
                variant='outline'
                size='sm'
                className='h-7 text-[11px]'
                onClick={() => void decide(proposals, false).catch(() => undefined)}
              >
                {t('work.rejectAll')}
              </Button>
            </>
          )}
        </div>
        {proposals.length === 0 ? (
          <p className='text-[11px] text-muted-foreground'>{t('work.glossary.noProposals')}</p>
        ) : (
          <>
            <p className='text-[11px] text-muted-foreground'>{t('work.glossary.editHint')}</p>
            <ul className='divide-y divide-border rounded-xl border border-border/80 bg-[var(--surface-panel)]'>
              {proposals.map((term, index) => (
                <li
                  key={term.source}
                  className='grid grid-cols-[minmax(0,0.8fr)_minmax(0,1fr)_minmax(0,1fr)_auto] items-center gap-3 px-4 py-2 text-[12px]'
                >
                  <span className='truncate' title={term.source}>
                    {term.source}
                  </span>
                  <Input
                    value={term.target}
                    aria-label={t('work.glossary.rendering', { source: term.source })}
                    onChange={(event) =>
                      setProposals((current) =>
                        current.map((item, at) =>
                          at === index ? { ...item, target: event.target.value } : item,
                        ),
                      )
                    }
                    className='h-7 text-[11px]'
                  />
                  <span className='truncate text-[10px] text-muted-foreground' title={term.note}>
                    {term.note}
                  </span>
                  <span className='flex gap-1'>
                    <Button
                      type='button'
                      size='icon-sm'
                      className='size-7'
                      aria-label={t('work.approve')}
                      title={t('work.approve')}
                      onClick={() => void decide([term], true).catch(() => undefined)}
                    >
                      <Check className='size-3.5' />
                    </Button>
                    <Button
                      type='button'
                      variant='outline'
                      size='icon-sm'
                      className='size-7'
                      aria-label={t('work.reject')}
                      title={t('work.reject')}
                      onClick={() => void decide([term], false).catch(() => undefined)}
                    >
                      <X className='size-3.5' />
                    </Button>
                  </span>
                </li>
              ))}
            </ul>
          </>
        )}
      </section>

      <section className='grid gap-3'>
        <h2 className='text-[12px] font-semibold'>{t('work.glossary.terms')}</h2>
        <p className='text-[11px] text-muted-foreground'>{t('work.glossary.termsHint')}</p>
        {work && work.terms.length > 0 && (
          <ul className='divide-y divide-border rounded-xl border border-border/80 bg-[var(--surface-panel)]'>
            {work.terms.map((term) => (
              <li
                key={term.source}
                className='grid grid-cols-[minmax(0,0.8fr)_minmax(0,1fr)_minmax(0,1fr)_auto] items-center gap-3 px-4 py-2 text-[12px]'
              >
                <span className='truncate'>{term.source}</span>
                <span className='truncate'>{term.target}</span>
                <span className='truncate text-[10px] text-muted-foreground'>{term.note}</span>
                <Button
                  type='button'
                  variant='ghost'
                  size='sm'
                  className='h-7 gap-1 text-[11px]'
                  onClick={() => promote(term)}
                >
                  <ArrowUpRight className='size-3.5' /> {t('work.glossary.toGlobal')}
                </Button>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  )
}
