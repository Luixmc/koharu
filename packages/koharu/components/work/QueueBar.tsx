'use client'

import { LoaderCircle, Square, X } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { useWorkStore } from '@/lib/work'
import { commands, type StepProgress } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Checkbox } from '@koharu/ui/components/checkbox'

/** What runs now, what waits, and the controls over the queue. */
export function QueueBar() {
  const { t } = useTranslation()
  const queue = useWorkStore((state) => state.queue)
  const running = queue?.running ?? null
  const now = useClock(running !== null)

  if (!queue) return null
  const busy = running !== null || queue.waiting.length > 0

  return (
    <footer className='grid shrink-0 gap-2 border-t border-border/80 bg-[var(--surface-panel)] px-8 py-3 text-[11px]'>
      <div className='flex items-center gap-3'>
        {running ? (
          <LoaderCircle className='size-3.5 shrink-0 animate-spin text-primary' />
        ) : (
          <span
            className={`size-2 shrink-0 rounded-full ${queue.failed ? 'bg-destructive' : 'bg-muted-foreground/40'}`}
          />
        )}
        <span className='min-w-0 flex-1 truncate font-medium'>
          {running ? running.name : queue.failed ? t('work.queue.failed') : t('work.queue.idle')}
        </span>
        <label className='flex items-center gap-2 text-muted-foreground'>
          <Checkbox
            checked={queue.shutdown_when_done}
            onCheckedChange={(checked) =>
              void call(commands.setShutdownWhenDone, checked === true).catch(() => undefined)
            }
          />
          {t('work.queue.shutdown')}
        </label>
        <Button
          type='button'
          variant='outline'
          size='sm'
          className='h-7 gap-1.5 text-[11px]'
          disabled={!busy}
          onClick={() => void call(commands.stopQueue).catch(() => undefined)}
        >
          <Square className='size-3' /> {t('work.queue.stop')}
        </Button>
      </div>
      {running && <ProgressLine progress={running.progress} started={running.started} now={now} />}
      {queue.waiting.length > 0 && (
        <div className='flex flex-wrap items-center gap-1.5 text-muted-foreground'>
          {t('work.queue.waiting')}
          {queue.waiting.map((project) => (
            <span
              key={project}
              className='flex items-center gap-1 rounded-full bg-muted py-0.5 pr-1 pl-2 text-foreground'
            >
              {project}
              <button
                type='button'
                className='grid size-4 place-items-center rounded-full hover:bg-foreground/10'
                aria-label={t('work.queue.remove', { project })}
                onClick={() => void call(commands.cancelQueued, project).catch(() => undefined)}
              >
                <X className='size-3' />
              </button>
            </span>
          ))}
        </div>
      )}
    </footer>
  )
}

function ProgressLine({
  progress,
  started,
  now,
}: {
  progress: StepProgress | null
  started: number
  now: number
}) {
  const { t } = useTranslation()
  const elapsed = duration(now - started)
  if (!progress) {
    return <p className='text-muted-foreground'>{t('work.queue.unmeasured', { elapsed })}</p>
  }
  const fraction = progress.total === 0 ? 0 : Math.min(progress.done / progress.total, 1)
  // The pace since the count first went up, so loading the model before the
  // first unit does not skew the estimate.
  const measured = progress.first ? progress.done - progress.first.done : 0
  const left =
    progress.first && measured > 0
      ? t('work.queue.left', {
          left: duration(((now - progress.first.at) * (progress.total - progress.done)) / measured),
        })
      : t('work.queue.estimating')
  return (
    <div className='grid gap-1'>
      <div className='h-1.5 overflow-hidden rounded-full bg-muted'>
        <div className='h-full bg-primary transition-all' style={{ width: `${fraction * 100}%` }} />
      </div>
      <p className='text-muted-foreground tabular-nums'>
        {t('work.queue.progress', {
          what: progress.what,
          done: progress.done,
          total: progress.total,
          elapsed,
          left,
        })}
      </p>
    </div>
  )
}

/** The current time, ticking every second while `running`. */
function useClock(running: boolean): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!running) return
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [running])
  return now
}

export function duration(milliseconds: number): string {
  const seconds = Math.max(0, Math.round(milliseconds / 1000))
  if (seconds < 60) return `${seconds} s`
  if (seconds < 3600)
    return `${Math.floor(seconds / 60)} min ${String(seconds % 60).padStart(2, '0')} s`
  return `${Math.floor(seconds / 3600)} h ${String(Math.floor((seconds % 3600) / 60)).padStart(2, '0')} min`
}
