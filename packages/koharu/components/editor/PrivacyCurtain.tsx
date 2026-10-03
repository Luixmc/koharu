'use client'

import { Eye } from 'lucide-react'
import { useTranslation } from 'react-i18next'

import { useKoharuStore } from '@/lib/store'
import { Button } from '@koharu/ui/components/button'
import { cn } from '@koharu/ui/lib/utils'

/**
 * Covers the canvas while the images are retracted. The canvas stays mounted
 * underneath, so jobs keep running and the view comes back where it was.
 */
export function PrivacyCurtain() {
  const { t } = useTranslation()
  const discreet = useKoharuStore((state) => state.discreet)
  const setDiscreet = useKoharuStore((state) => state.setDiscreet)
  const jobs = useKoharuStore((state) => state.jobs)
  const running = Object.values(jobs).filter((job) => job.state === 'running')
  const done = running.reduce((sum, job) => sum + job.completed, 0)
  const total = running.reduce((sum, job) => sum + job.total, 0)

  return (
    <div
      aria-hidden={!discreet}
      inert={!discreet}
      className={cn(
        'absolute inset-0 z-20 grid place-items-center bg-[var(--surface-canvas)] transition-transform duration-300 ease-out motion-reduce:transition-none',
        discreet ? 'translate-y-0' : '-translate-y-full',
      )}
    >
      <div className='flex flex-col items-center gap-3 text-center'>
        <span className='text-[13px] font-medium'>{t('privacy.hidden')}</span>
        {running.length > 0 && (
          <span className='text-[11px] text-muted-foreground tabular-nums'>
            {t('privacy.working', { done, total })}
          </span>
        )}
        <Button variant='outline' size='sm' onClick={() => setDiscreet(false)}>
          <Eye />
          {t('privacy.show')}
        </Button>
      </div>
    </div>
  )
}
