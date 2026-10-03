'use client'

import { BookOpen, ExternalLink, FolderOpen } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'

import { call } from '@/lib/backend'
import { useWork } from '@/lib/work'
import { commands } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'

/** The ficha `khr estudiar` wrote: characters, relationships and tone. */
export function FichaTab({ project }: { project: string }) {
  const { t } = useTranslation()
  const work = useWork(project).data
  const open = (file: 'ficha' | 'folder') =>
    void call(commands.openWorkFile, project, file).catch(() => undefined)

  return (
    <div className='grid gap-5'>
      <div className='flex flex-wrap gap-2'>
        <Button
          type='button'
          variant='outline'
          size='sm'
          className='h-8 gap-1.5 text-[11px]'
          disabled={!work?.ficha}
          onClick={() => open('ficha')}
        >
          <ExternalLink className='size-3.5' /> {t('work.ficha.openExternal')}
        </Button>
        <Button
          type='button'
          variant='ghost'
          size='sm'
          className='h-8 gap-1.5 text-[11px]'
          onClick={() => open('folder')}
        >
          <FolderOpen className='size-3.5' /> {t('work.ficha.openFolder')}
        </Button>
      </div>

      {work && (work.notes.tags.length > 0 || work.notes.description) && (
        <section className='rounded-xl border border-border/80 bg-[var(--surface-panel)] p-4 text-[11px]'>
          <h2 className='mb-2 text-[12px] font-semibold'>{t('work.ficha.yourNotes')}</h2>
          {work.notes.description && (
            <p className='mb-2 whitespace-pre-wrap'>{work.notes.description}</p>
          )}
          <div className='flex flex-wrap gap-1'>
            {work.notes.tags.map((tag) => (
              <span key={tag} className='rounded-full bg-muted px-2 py-0.5'>
                {tag}
              </span>
            ))}
          </div>
        </section>
      )}

      {work?.ficha ? (
        <article className='min-w-0 text-[12px] leading-6 break-words [&_blockquote]:my-2 [&_blockquote]:border-l-2 [&_blockquote]:border-border [&_blockquote]:pl-3 [&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_h1]:mt-6 [&_h1]:mb-2 [&_h1]:text-[16px] [&_h1]:font-semibold [&_h2]:mt-6 [&_h2]:mb-2 [&_h2]:text-[14px] [&_h2]:font-semibold [&_h3]:mt-4 [&_h3]:mb-1 [&_h3]:font-semibold [&_li]:my-0.5 [&_ol]:my-2 [&_ol]:list-decimal [&_ol]:pl-5 [&_p]:my-2 [&_table]:my-2 [&_table]:w-full [&_td]:border [&_td]:border-border [&_td]:p-1.5 [&_th]:border [&_th]:border-border [&_th]:p-1.5 [&_th]:text-left [&_ul]:my-2 [&_ul]:list-disc [&_ul]:pl-5 [&>*:first-child]:mt-0'>
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{work.ficha}</ReactMarkdown>
        </article>
      ) : (
        <div className='grid place-items-center rounded-xl border border-dashed border-border py-16 text-center'>
          <BookOpen className='size-6 text-muted-foreground' />
          <p className='mt-3 text-[12px] font-medium'>{t('work.ficha.emptyTitle')}</p>
          <p className='mt-1 max-w-[46ch] text-[11px] text-muted-foreground'>
            {t('work.ficha.emptyDescription')}
          </p>
        </div>
      )}
    </div>
  )
}
