'use client'

import { useQuery } from '@tanstack/react-query'
import { CircleCheck, Download, LoaderCircle } from 'lucide-react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { ocrName, recommendationsQuery, useWorkStore } from '@/lib/work'
import { commands } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'

/**
 * What khr recommends: the OCR it uses for each source language and its
 * catalog of LM Studio models. Only what the test bench measured is shown as
 * a result; the rest is labeled untested.
 */
export function ModelsTab() {
  const { t } = useTranslation()
  const { data, isPending, error } = useQuery(recommendationsQuery)
  const running = useWorkStore((state) => state.queue?.running?.name ?? '')

  if (isPending) {
    return <LoaderCircle className='size-4 animate-spin text-muted-foreground' />
  }
  if (error || !data) {
    return <p className='text-[12px] text-muted-foreground'>{t('work.models.unavailable')}</p>
  }
  // Measured models first, then installed ones.
  const models = [...data.models].sort(
    (a, b) =>
      Number(Object.keys(b.measured).length > 0) - Number(Object.keys(a.measured).length > 0) ||
      Number(b.installed) - Number(a.installed),
  )

  return (
    <div className='grid gap-8'>
      <section className='grid gap-3'>
        <h2 className='text-[12px] font-semibold'>{t('work.models.ocrTitle')}</h2>
        <p className='text-[11px] text-muted-foreground'>{t('work.models.ocrDescription')}</p>
        <ul className='grid grid-cols-2 gap-2 sm:grid-cols-4'>
          {Object.entries(data.ocr).map(([language, ocr]) => (
            <li
              key={language}
              className='rounded-xl border border-border/80 bg-[var(--surface-panel)] px-4 py-3'
            >
              <p className='text-[10px] text-muted-foreground'>{t(`work.languages.${language}`)}</p>
              <p className='text-[12px] font-medium'>{ocrName(ocr)}</p>
            </li>
          ))}
        </ul>
      </section>

      <section className='grid gap-3'>
        <h2 className='text-[12px] font-semibold'>{t('work.models.llmTitle')}</h2>
        <p className='text-[11px] text-muted-foreground'>{t('work.models.llmDescription')}</p>
        <ul className='divide-y divide-border rounded-xl border border-border/80 bg-[var(--surface-panel)]'>
          {models.map((model) => {
            const measured = Object.entries(model.measured)
            const downloading = running.includes(model.id)
            return (
              <li key={model.id} className='grid gap-1 px-4 py-3 text-[12px]'>
                <div className='flex flex-wrap items-center gap-2'>
                  <span className='font-medium'>{model.id}</span>
                  {model.tasks.map((task) => (
                    <span key={task} className='rounded bg-muted px-1.5 text-[10px]'>
                      {t(`work.models.task.${task}`, { defaultValue: task })}
                    </span>
                  ))}
                  <span className='text-[10px] text-muted-foreground tabular-nums'>
                    {model.gb.toFixed(1)} GB
                  </span>
                  {model.installed ? (
                    <span className='ml-auto flex items-center gap-1 text-[11px] text-primary'>
                      <CircleCheck className='size-3.5' /> {t('work.models.installed')}
                    </span>
                  ) : (
                    <Button
                      type='button'
                      variant='outline'
                      size='sm'
                      className='ml-auto h-7 gap-1.5 text-[11px]'
                      disabled={downloading}
                      onClick={() =>
                        void call(commands.downloadModel, model.id).catch(() => undefined)
                      }
                    >
                      {downloading ? (
                        <LoaderCircle className='size-3 animate-spin' />
                      ) : (
                        <Download className='size-3' />
                      )}
                      {t('work.models.download')}
                    </Button>
                  )}
                </div>
                {measured.length > 0 ? (
                  measured.map(([task, verdict]) => (
                    <p key={task} className='text-[11px] text-primary'>
                      {t('work.models.measured', {
                        task: t(`work.models.task.${task}`, { defaultValue: task }),
                        verdict,
                      })}
                    </p>
                  ))
                ) : (
                  <p className='text-[11px] text-muted-foreground'>{t('work.models.untested')}</p>
                )}
                <p className='text-[11px] text-muted-foreground'>{model.note}</p>
              </li>
            )
          })}
        </ul>
      </section>
    </div>
  )
}
