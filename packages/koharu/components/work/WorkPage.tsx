'use client'

import {
  ArrowLeft,
  BookOpen,
  Boxes,
  FolderInput,
  Layers,
  ListChecks,
  PencilLine,
  Play,
  SquarePen,
} from 'lucide-react'
import { useEffect } from 'react'
import { useTranslation } from 'react-i18next'

import { CorrectionsTab } from '@/components/work/CorrectionsTab'
import { FichaTab } from '@/components/work/FichaTab'
import { GlossaryTab } from '@/components/work/GlossaryTab'
import { ModelsTab } from '@/components/work/ModelsTab'
import { ProcessTab } from '@/components/work/ProcessTab'
import { QueueBar } from '@/components/work/QueueBar'
import { call } from '@/lib/backend'
import { pageKey, pagesKey, projectKey, refresh, useProject } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { useProjectList, useWork, useWorkStore, type WorkTab } from '@/lib/work'
import { commands, type NextStep } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@koharu/ui/components/select'

const tabs = [
  ['process', Play],
  ['batch', Layers],
  ['ficha', BookOpen],
  ['glossary', ListChecks],
  ['corrections', PencilLine],
  ['models', Boxes],
] as const

/**
 * The work khr does around a project outside the editor: the steps and their
 * queue, the ficha, the glossary, the reviewer's corrections and the models.
 */
export function WorkPage() {
  const { t } = useTranslation()
  const setWorkOpen = useKoharuStore((state) => state.setWorkOpen)
  const openProject = useProject().data
  const projects = useProjectList().data ?? []
  const project = useWorkStore((state) => state.project)
  const setProject = useWorkStore((state) => state.setProject)
  const tab = useWorkStore((state) => state.tab)
  const setTab = useWorkStore((state) => state.setTab)
  const queue = useWorkStore((state) => state.queue)
  const busy = queue?.busy ?? []
  const work = useWork(project).data

  const firstProject = projects[0]?.name
  useEffect(() => {
    if (project !== null) return
    const initial = openProject?.name ?? firstProject
    if (initial) setProject(initial)
  }, [firstProject, openProject?.name, project, setProject])

  const edit = async () => {
    if (!project) return
    if (openProject?.name !== project) {
      await call(commands.openProject, project)
      await refresh(projectKey, pagesKey, pageKey)
    }
    setWorkOpen(false)
  }

  return (
    <section className='flex min-h-0 flex-1 bg-[var(--surface-sidebar)] text-foreground'>
      <nav className='flex w-64 shrink-0 flex-col bg-[var(--surface-sidebar)] px-3 py-4'>
        <Button
          type='button'
          variant='ghost'
          className='mb-5 h-9 justify-start gap-2 rounded-lg px-2 text-[12px] text-muted-foreground hover:bg-foreground/[0.06] hover:text-foreground'
          onClick={() => setWorkOpen(false)}
        >
          <ArrowLeft className='size-4' /> {t('work.back')}
        </Button>
        <p className='mb-2 px-2 text-[10px] font-semibold tracking-[0.14em] text-muted-foreground uppercase'>
          {t('work.project')}
        </p>
        <Select
          value={project ?? ''}
          items={Object.fromEntries(projects.map(({ name }) => [name, name]))}
          onValueChange={(name) => name && setProject(name)}
        >
          <SelectTrigger aria-label={t('work.project')} className='h-9 w-full text-[11px]'>
            <SelectValue placeholder={t('work.noProject')} />
          </SelectTrigger>
          <SelectContent>
            {projects.map(({ name }) => (
              <SelectItem key={name} value={name}>
                {name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button
          type='button'
          variant='ghost'
          size='sm'
          className='mt-1 h-8 justify-start gap-2 px-2 text-[11px] text-muted-foreground'
          title={t('work.fromFolderHint')}
          onClick={() => void call(commands.createProjectFromFolder).catch(() => undefined)}
        >
          <FolderInput className='size-3.5' /> {t('work.fromFolder')}
        </Button>

        <div className='mt-5 grid gap-1'>
          {tabs.map(([id, Icon]) => (
            <Button
              key={id}
              type='button'
              variant='ghost'
              size='sm'
              data-active={tab === id}
              className='h-10 w-full justify-start gap-3 rounded-lg px-3 text-left text-[12px] text-muted-foreground hover:bg-foreground/[0.06] hover:text-foreground data-[active=true]:bg-accent data-[active=true]:text-accent-foreground'
              onClick={() => setTab(id)}
            >
              <Icon className='size-4' /> {t(`work.tabs.${id}`)}
              <TabCount tab={id} />
            </Button>
          ))}
        </div>
        {work && project && (
          <p className='mt-auto px-2 pt-6 text-[11px] leading-4 text-muted-foreground'>
            <NextStepText next={work.next} />
          </p>
        )}
      </nav>

      <main className='relative z-10 flex min-w-0 flex-1 flex-col overflow-hidden rounded-tl-2xl bg-[var(--surface-canvas)] shadow-[var(--shadow-content)]'>
        <header className='flex h-14 shrink-0 items-center gap-3 border-b border-border/80 px-8'>
          <h1 className='min-w-0 truncate text-[13px] font-semibold tracking-[-0.02em]'>
            {tab === 'batch' ? t('work.batch.title') : (project ?? t('work.noProject'))}
          </h1>
          {project && tab !== 'batch' && (
            <Button
              type='button'
              variant='outline'
              size='sm'
              className='ml-auto h-8 gap-1.5 text-[11px]'
              disabled={busy.includes(project)}
              title={busy.includes(project) ? t('work.editBusy') : undefined}
              onClick={() => void edit().catch(() => undefined)}
            >
              <SquarePen className='size-3.5' /> {t('work.edit')}
            </Button>
          )}
        </header>
        <ScrollArea className='min-h-0 flex-1'>
          <div className='mx-auto w-full max-w-5xl px-10 py-8'>
            {tab === 'batch' && <ProcessTab project={null} batch />}
            {project === null ? (
              tab !== 'batch' && (
                <p className='text-[12px] text-muted-foreground'>{t('work.noProjectHint')}</p>
              )
            ) : (
              <>
                {/* Kept mounted: the ticked steps and notes survive a tab switch. */}
                <div className={tab === 'process' ? undefined : 'hidden'}>
                  <ProcessTab project={project} />
                </div>
                {tab === 'ficha' && <FichaTab project={project} />}
                {tab === 'glossary' && <GlossaryTab project={project} />}
                {tab === 'corrections' && <CorrectionsTab project={project} />}
              </>
            )}
            {tab === 'models' && <ModelsTab />}
          </div>
        </ScrollArea>
        <QueueBar />
      </main>
    </section>
  )
}

function TabCount({ tab }: { tab: WorkTab }) {
  const project = useWorkStore((state) => state.project)
  const work = useWork(project).data
  const count =
    tab === 'glossary'
      ? work?.proposals.length
      : tab === 'corrections'
        ? work?.corrections.length
        : undefined
  if (!count) return null
  return (
    <span className='ml-auto rounded-full bg-primary px-1.5 text-[9px] text-primary-foreground tabular-nums'>
      {count}
    </span>
  )
}

function NextStepText({ next }: { next: NextStep }) {
  const { t } = useTranslation()
  switch (next.kind) {
    case 'process':
      return t('work.next.process')
    case 'proposals':
      return t('work.next.proposals', { count: next.count })
    case 'corrections':
      return t('work.next.corrections', { count: next.count })
    case 'apply':
      return t('work.next.apply')
    case 'finish':
      return t('work.next.finish')
  }
}
