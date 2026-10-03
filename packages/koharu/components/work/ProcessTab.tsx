'use client'

import { useQuery } from '@tanstack/react-query'
import {
  Cpu,
  Download,
  FolderOpen,
  FolderTree,
  GraduationCap,
  LoaderCircle,
  Play,
  Plus,
  RotateCcw,
  X,
} from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { queryClient } from '@/lib/queries'
import {
  languageOf,
  llmModelsQuery,
  modelChoicesKey,
  modelChoicesQuery,
  modelTasks,
  ocrName,
  recommendationsQuery,
  useProjectList,
  useWork,
  useWorkStore,
  withInstalledModels,
} from '@/lib/work'
import {
  commands,
  type CatalogModel,
  type KoharuStage,
  type ModelChoices,
  type SourceLanguage,
} from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Checkbox } from '@koharu/ui/components/checkbox'
import { Input } from '@koharu/ui/components/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@koharu/ui/components/select'
import { Textarea } from '@koharu/ui/components/textarea'

const stages: readonly KoharuStage[] = ['detection', 'ocr', 'inpainting']
const languages: readonly SourceLanguage[] = ['ja', 'ko', 'zh', 'en']
const deepl = '__deepl__'

type ModelRole = keyof typeof modelTasks

/**
 * The steps and their options for the project on the page or, in batch mode,
 * for a list of folders that each become a project.
 */
export function ProcessTab({
  project,
  batch = false,
}: {
  project: string | null
  batch?: boolean
}) {
  const { t } = useTranslation()
  const work = useWork(project).data
  const queue = useWorkStore((state) => state.queue)
  const busy = queue !== null && (queue.running !== null || queue.waiting.length > 0)
  const recommendations = useQuery(recommendationsQuery).data

  const [ticked, setTicked] = useState<KoharuStage[]>([...stages])
  const [study, setStudy] = useState(true)
  const [translate, setTranslate] = useState(true)
  const [review, setReview] = useState(true)
  const [gallery, setGallery] = useState('')
  const [fetching, setFetching] = useState(false)
  const [tags, setTags] = useState('')
  const [description, setDescription] = useState('')
  const saved = useQuery(modelChoicesQuery).data
  // LM Studio closed: the saved choices stand as they are.
  const listed = useQuery(llmModelsQuery)
  const installed = listed.data ?? (listed.isError ? [] : undefined)
  const models =
    saved && installed ? withInstalledModels(saved, installed, recommendations?.models ?? []) : null
  const [pages, setPages] = useState(0)
  const [language, setLanguage] = useState<SourceLanguage>('ja')
  const leftToRight = useWorkStore((state) => state.leftToRight)
  const setLeftToRight = useWorkStore((state) => state.setLeftToRight)
  const loadedNotes = useRef<string | null>(null)
  const folders = useWorkStore((state) => state.folders)
  const setFolders = useWorkStore((state) => state.setFolders)

  // Everything about the work belongs to it: a switch reloads it.
  useEffect(() => {
    setGallery('')
    if (!project) return
    const guessed = languageOf(project)
    if (guessed) setLanguage(guessed)
  }, [project])
  useEffect(() => {
    if (!project || !work || loadedNotes.current === project) return
    loadedNotes.current = project
    setTags(work.notes.tags.join(', '))
    setDescription(work.notes.description)
  }, [project, work])

  const notes = () => ({
    tags: tags
      .split(/[,;\n]/)
      .map((tag) => tag.trim())
      .filter(Boolean),
    description: description.trim(),
  })
  const saveNotes = () => {
    if (project) void call(commands.saveUserNotes, project, notes()).catch(() => undefined)
  }

  const changeModels = (next: ModelChoices) => {
    queryClient.setQueryData(modelChoicesKey, next)
    void call(commands.saveModelChoices, next).catch(() => undefined)
  }

  const fetchGallery = async () => {
    if (!gallery.trim() || fetching) return
    setFetching(true)
    try {
      const found = await call(commands.fetchGallery, gallery)
      const known = notes().tags
      setTags([...known, ...found.tags.filter((tag) => !known.includes(tag))].join(', '))
      const title = found.original_title || found.title
      if (!description.trim() && title) setDescription(t('work.process.titleNote', { title }))
    } finally {
      setFetching(false)
    }
  }

  const addFolders = async (inside: boolean) => {
    const picked = await call(commands.pickBatchFolders, inside)
    setFolders((current) => [...current, ...picked.filter((folder) => !current.includes(folder))])
  }

  const run = () => {
    if (!models) return
    const plan = {
      project: project ?? '',
      stages: stages.filter((stage) => ticked.includes(stage)),
      study,
      gallery,
      notes: notes(),
      translate,
      review,
      models,
      pages,
      language,
      left_to_right: leftToRight,
    }
    if (batch) {
      void call(commands.enqueueBatch, folders, plan)
        .then(() => setFolders(() => []))
        .catch(() => undefined)
    } else {
      void call(commands.enqueue, plan).catch(() => undefined)
    }
  }

  return (
    <div className='grid gap-8'>
      {queue && queue.unfinished > 0 && (
        <div className='flex items-center gap-3 rounded-xl border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-[12px]'>
          <span className='min-w-0 flex-1'>
            {t('work.process.unfinished', {
              count: queue.unfinished,
              projects: queue.unfinished_projects.join(', '),
            })}
          </span>
          <Button
            type='button'
            size='sm'
            className='h-7 gap-1.5 text-[11px]'
            onClick={() => void call(commands.resumeQueue).catch(() => undefined)}
          >
            <RotateCcw className='size-3' /> {t('work.process.resume')}
          </Button>
          <Button
            type='button'
            variant='ghost'
            size='sm'
            className='h-7 text-[11px]'
            onClick={() => void call(commands.discardUnfinishedQueue).catch(() => undefined)}
          >
            {t('work.process.discard')}
          </Button>
        </div>
      )}

      {batch && (
        <FolderList
          folders={folders}
          onAdd={(inside) => void addFolders(inside).catch(() => undefined)}
          onRemove={(folder) =>
            setFolders((current) => current.filter((value) => value !== folder))
          }
          onClear={() => setFolders(() => [])}
        />
      )}

      <div className='grid gap-8 lg:grid-cols-2'>
        <section className='grid content-start gap-3'>
          <h2 className='text-[12px] font-semibold'>{t('work.process.steps')}</h2>
          {stages.map((stage) => (
            <Tick
              key={stage}
              checked={ticked.includes(stage)}
              onChange={(on) =>
                setTicked((current) =>
                  on ? [...current, stage] : current.filter((value) => value !== stage),
                )
              }
              label={t(`work.process.stage.${stage}`)}
            />
          ))}
          <Tick checked={study} onChange={setStudy} label={t('work.process.study')} />
          {study && batch && (
            <p className='ml-6 text-[11px] text-muted-foreground'>{t('work.batch.studyHint')}</p>
          )}
          {study && !batch && (
            <div className='ml-6 grid gap-2 rounded-xl border border-border/80 bg-[var(--surface-panel)] p-3'>
              <p className='text-[11px] text-muted-foreground'>{t('work.process.notesHint')}</p>
              <div className='flex gap-2'>
                <Input
                  value={gallery}
                  onChange={(event) => setGallery(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') void fetchGallery().catch(() => undefined)
                  }}
                  placeholder={t('work.process.galleryPlaceholder')}
                  aria-label={t('work.process.galleryPlaceholder')}
                  className='h-8 text-[11px]'
                />
                <Button
                  type='button'
                  variant='outline'
                  size='sm'
                  className='h-8 shrink-0 gap-1.5 text-[11px]'
                  disabled={!gallery.trim() || fetching}
                  onClick={() => void fetchGallery().catch(() => undefined)}
                >
                  {fetching && <LoaderCircle className='size-3 animate-spin' />}
                  {t('work.process.fetchTags')}
                </Button>
              </div>
              <Input
                value={tags}
                onChange={(event) => setTags(event.target.value)}
                onBlur={saveNotes}
                placeholder={t('work.process.tagsPlaceholder')}
                aria-label={t('work.process.tagsPlaceholder')}
                className='h-8 text-[11px]'
              />
              <Textarea
                value={description}
                onChange={(event) => setDescription(event.target.value)}
                onBlur={saveNotes}
                placeholder={t('work.process.descriptionPlaceholder')}
                aria-label={t('work.process.descriptionPlaceholder')}
                rows={3}
                className='text-[11px]'
              />
            </div>
          )}
          <Tick checked={translate} onChange={setTranslate} label={t('work.process.translate')} />
          <Tick checked={review} onChange={setReview} label={t('work.process.review')} />
        </section>

        <section className='grid content-start gap-3'>
          <h2 className='text-[12px] font-semibold'>{t('work.process.options')}</h2>
          {models && (
            <>
              <ModelRow
                role='study'
                label={t('work.process.studyModel')}
                value={models.study}
                onChange={(study) => changeModels({ ...models, study })}
              />
              <ModelRow
                role='translation'
                label={t('work.process.translationModel')}
                value={models.deepl ? deepl : models.translation}
                onChange={(choice) =>
                  changeModels(
                    choice === deepl
                      ? { ...models, deepl: true }
                      : { ...models, deepl: false, translation: choice },
                  )
                }
              />
              <ModelRow
                role='review'
                label={t('work.process.reviewModel')}
                value={models.review}
                onChange={(review) => changeModels({ ...models, review })}
              />
            </>
          )}
          <Field label={batch ? t('work.batch.language') : t('work.process.language')}>
            <Select
              value={language}
              items={Object.fromEntries(
                languages.map((code) => [code, t(`work.languages.${code}`)]),
              )}
              onValueChange={(code) => code && setLanguage(code as SourceLanguage)}
            >
              <SelectTrigger className='h-8 w-full text-[11px]'>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {languages.map((code) => (
                  <SelectItem key={code} value={code}>
                    {t(`work.languages.${code}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
          {batch && (
            <p className='-mt-1 text-[10px] text-muted-foreground'>
              {t('work.batch.languageHint')}
            </p>
          )}
          {recommendations?.ocr[language] && (
            <p className='-mt-1 text-[10px] text-muted-foreground'>
              {t('work.process.ocrHint', { ocr: ocrName(recommendations.ocr[language]) })}
            </p>
          )}
          <Field label={t('work.process.pages')}>
            <Input
              type='number'
              min={0}
              value={pages}
              onChange={(event) => setPages(Math.max(0, Number(event.target.value) || 0))}
              className='h-8 w-24 text-[11px]'
            />
          </Field>
          <Tick
            checked={leftToRight}
            onChange={setLeftToRight}
            label={t('work.process.leftToRight')}
          />
        </section>
      </div>

      <div className='flex flex-wrap items-center gap-2'>
        <Button
          type='button'
          size='sm'
          className='h-9 gap-1.5 text-[12px]'
          disabled={!models || (batch && folders.length === 0)}
          onClick={run}
        >
          {busy ? <Plus className='size-3.5' /> : <Play className='size-3.5' />}
          {batch
            ? t('work.batch.run', { count: folders.length })
            : busy
              ? t('work.process.enqueue')
              : t('work.process.run')}
        </Button>
        {batch && (
          <span className='text-[11px] text-muted-foreground'>{t('work.batch.failureHint')}</span>
        )}
        {!batch && project && (
          <>
            <Button
              type='button'
              variant='outline'
              size='sm'
              className='h-9 gap-1.5 text-[12px]'
              disabled={!models}
              title={t('work.process.learnHint')}
              onClick={() =>
                models &&
                void call(commands.learnFromCorrections, project, models.translation).catch(
                  () => undefined,
                )
              }
            >
              <GraduationCap className='size-3.5' /> {t('work.process.learn')}
            </Button>
            <Button
              type='button'
              variant='outline'
              size='sm'
              className='h-9 gap-1.5 text-[12px]'
              title={t('work.process.exportHint')}
              onClick={() => void call(commands.exportPages, project).catch(() => undefined)}
            >
              <Download className='size-3.5' /> {t('work.process.export')}
            </Button>
          </>
        )}
        <Button
          type='button'
          variant='ghost'
          size='sm'
          className='h-9 gap-1.5 text-[12px]'
          onClick={() => void call(commands.freeMemory).catch(() => undefined)}
        >
          <Cpu className='size-3.5' /> {t('work.process.freeMemory')}
        </Button>
      </div>

      <Log />
    </div>
  )
}

/** The folders of a batch; those whose project exists are marked. */
function FolderList({
  folders,
  onAdd,
  onRemove,
  onClear,
}: {
  folders: string[]
  onAdd: (inside: boolean) => void
  onRemove: (folder: string) => void
  onClear: () => void
}) {
  const { t } = useTranslation()
  const projects = useProjectList().data ?? []
  const nameOf = (folder: string) => folder.split(/[\\/]/).filter(Boolean).pop() ?? folder

  return (
    <section className='grid gap-3'>
      <div className='grid gap-1'>
        <h2 className='text-[12px] font-semibold'>{t('work.batch.title')}</h2>
        <p className='text-[11px] text-muted-foreground'>{t('work.batch.intro')}</p>
      </div>
      <div className='flex flex-wrap items-center gap-2'>
        <Button
          type='button'
          variant='outline'
          size='sm'
          className='h-8 gap-1.5 text-[11px]'
          onClick={() => onAdd(false)}
        >
          <FolderOpen className='size-3.5' /> {t('work.batch.addFolders')}
        </Button>
        <Button
          type='button'
          variant='outline'
          size='sm'
          className='h-8 gap-1.5 text-[11px]'
          title={t('work.batch.addInsideHint')}
          onClick={() => onAdd(true)}
        >
          <FolderTree className='size-3.5' /> {t('work.batch.addInside')}
        </Button>
        <Button
          type='button'
          variant='ghost'
          size='sm'
          className='h-8 text-[11px]'
          disabled={folders.length === 0}
          onClick={onClear}
        >
          {t('work.batch.clear')}
        </Button>
      </div>
      <div className='rounded-xl border border-border/80 bg-[var(--surface-panel)] p-2'>
        {folders.length === 0 && (
          <p className='px-1 py-1 text-[11px] text-muted-foreground'>{t('work.batch.empty')}</p>
        )}
        {folders.map((folder) => {
          const name = nameOf(folder)
          return (
            <div key={folder} className='flex items-center gap-2 px-1 py-0.5 text-[11px]'>
              <span className='font-medium'>{name}</span>
              {projects.some((project) => project.name === name) && (
                <span className='rounded bg-primary/15 px-1 text-[9px] text-primary'>
                  {t('work.batch.exists')}
                </span>
              )}
              <span className='min-w-0 flex-1 truncate text-muted-foreground'>{folder}</span>
              <Button
                type='button'
                variant='ghost'
                size='icon'
                className='size-6'
                aria-label={t('work.batch.remove', { folder: name })}
                onClick={() => onRemove(folder)}
              >
                <X className='size-3' />
              </Button>
            </div>
          )
        })}
      </div>
    </section>
  )
}

function Tick({
  checked,
  onChange,
  label,
}: {
  checked: boolean
  onChange: (checked: boolean) => void
  label: string
}) {
  return (
    <label className='flex items-center gap-2.5 text-[12px]'>
      <Checkbox checked={checked} onCheckedChange={(value) => onChange(value === true)} />
      {label}
    </label>
  )
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className='grid grid-cols-[140px_minmax(0,1fr)] items-center gap-3 text-[11px]'>
      <span className='text-muted-foreground'>{label}</span>
      {children}
    </div>
  )
}

/**
 * A menu of the installed LM Studio models for one step, with what khr's
 * catalog measured about the chosen one.
 */
function ModelRow({
  role,
  label,
  value,
  onChange,
}: {
  role: ModelRole
  label: string
  value: string
  onChange: (value: string) => void
}) {
  const { t } = useTranslation()
  const installed = useQuery(llmModelsQuery).data ?? []
  const catalog = useQuery(recommendationsQuery).data?.models ?? []
  const setTab = useWorkStore((state) => state.setTab)
  const task = modelTasks[role]
  // A saved model that is no longer installed stays visible, so the menu
  // never shows a choice that differs from what would run.
  const names = [...installed]
  if (value && value !== deepl && !names.includes(value)) names.push(value)
  const entry = (name: string): CatalogModel | undefined =>
    catalog.find((model) => model.id === name)
  const measured = (name: string) => entry(name)?.measured[task]
  const items: Record<string, string> = Object.fromEntries(names.map((name) => [name, name]))
  if (role === 'translation') items[deepl] = t('work.process.deepl')
  const chosen = value === deepl ? undefined : entry(value)
  const suggested = catalog.filter(
    (model) => !model.installed && model.tasks.includes(task) && model.measured[task],
  )

  return (
    <div className='grid gap-1'>
      <Field label={label}>
        <Select value={value} items={items} onValueChange={(next) => next && onChange(next)}>
          <SelectTrigger className='h-8 w-full text-[11px]'>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {role === 'translation' && <SelectItem value={deepl}>{items[deepl]}</SelectItem>}
            {names.map((name) => (
              <SelectItem key={name} value={name}>
                <span className='flex w-full items-center gap-2'>
                  {name}
                  {measured(name) && (
                    <span className='ml-auto rounded bg-primary/15 px-1 text-[9px] text-primary'>
                      {t('work.process.recommended')}
                    </span>
                  )}
                </span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Field>
      <p className='ml-[152px] text-[10px] leading-4 text-muted-foreground'>
        {chosen
          ? (chosen.measured[task] ?? t('work.process.unmeasured'))
          : value !== deepl && value
            ? t('work.process.notInCatalog')
            : null}
        {suggested.length > 0 && (
          <>
            {' '}
            <button
              type='button'
              className='text-primary underline'
              onClick={() => setTab('models')}
            >
              {t('work.process.suggested', {
                models: suggested.map((model) => model.id).join(', '),
              })}
            </button>
          </>
        )}
      </p>
    </div>
  )
}

/** The steps' output, newest at the bottom. */
function Log() {
  const { t } = useTranslation()
  const log = useWorkStore((state) => state.log)
  // Scrolls the log alone, not the page around it.
  const box = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (box.current) box.current.scrollTop = box.current.scrollHeight
  }, [log])

  return (
    <section className='grid gap-2'>
      <h2 className='text-[12px] font-semibold'>{t('work.process.log')}</h2>
      <div
        ref={box}
        className='h-80 overflow-auto rounded-xl border border-border/80 bg-[var(--surface-panel)] p-3 font-mono text-[11px] leading-[1.45]'
      >
        {log.length === 0 && <p className='text-muted-foreground'>{t('work.process.emptyLog')}</p>}
        {log.map((line, index) => (
          <p
            key={index}
            className={
              line.startsWith('!!!')
                ? 'text-destructive'
                : line.startsWith('===')
                  ? 'font-semibold'
                  : line.startsWith('+ ')
                    ? 'text-green-600 dark:text-green-400'
                    : line.startsWith('- ')
                      ? 'text-orange-600 dark:text-orange-300'
                      : undefined
            }
          >
            {line || ' '}
          </p>
        ))}
      </div>
    </section>
  )
}
