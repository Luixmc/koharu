'use client'

import { queryOptions, useQuery } from '@tanstack/react-query'
import { Channel } from '@tauri-apps/api/core'
import { create } from 'zustand'

import {
  commands,
  type QueueEvent,
  type QueueState,
  type SourceLanguage,
} from '@koharu/bridge/protocol'

import { call } from './backend'
import { queryClient, refresh } from './queries'

const maxLines = 5000

export type WorkTab = 'process' | 'ficha' | 'glossary' | 'corrections' | 'models'

interface WorkStore {
  queue: QueueState | null
  log: string[]
  /** The project the work page shows. */
  project: string | null
  tab: WorkTab
  /** Western reading order; unticked is manga order. */
  leftToRight: boolean
  setProject: (project: string | null) => void
  setTab: (tab: WorkTab) => void
  setLeftToRight: (leftToRight: boolean) => void
}

export const useWorkStore = create<WorkStore>()((set) => ({
  queue: null,
  log: [],
  project: null,
  tab: 'process',
  leftToRight: false,
  setProject: (project) => set({ project }),
  setTab: (tab) => set({ tab }),
  setLeftToRight: (leftToRight) => set({ leftToRight }),
}))

export const workKey = (project: string) => ['work', project] as const
export const projectListKey = ['project-list'] as const
export const llmModelsKey = ['llm-models'] as const
export const recommendationsKey = ['recommendations'] as const

export function useWork(project: string | null) {
  return useQuery({
    queryKey: workKey(project ?? ''),
    queryFn: () => call(commands.getWork, project ?? ''),
    enabled: project !== null,
  })
}

export function useProjectList() {
  return useQuery({ queryKey: projectListKey, queryFn: () => call(commands.listProjects) })
}

export const llmModelsQuery = queryOptions({
  queryKey: llmModelsKey,
  queryFn: () => call(commands.getLlmModels),
})

export const recommendationsQuery = queryOptions({
  queryKey: recommendationsKey,
  queryFn: () => call(commands.getRecommendations),
})

/** Projects named "... JA", "... KO", "... ZH" or "... EN" set their language. */
export function languageOf(project: string): SourceLanguage | null {
  const match = /[\s-](ja|ko|zh|en)$/i.exec(project)
  return match ? (match[1].toLowerCase() as SourceLanguage) : null
}

const ocrNames: Record<string, string> = {
  'baberu-ocr': 'Baberu OCR',
  'hayai-ocr': 'Hayai OCR',
  'manga-ocr': 'Manga OCR',
  'paddleocr-vl-1.6': 'PaddleOCR-VL 1.6',
}

export function ocrName(model: string): string {
  return ocrNames[model] ?? model
}

function receive(event: QueueEvent): void {
  switch (event.type) {
    case 'state': {
      const previous = useWorkStore.getState().queue
      useWorkStore.setState({ queue: event.state })
      // A finished queue may have downloaded models or changed a project.
      if (previous?.running && !event.state.running && event.state.waiting.length === 0) {
        void refresh(llmModelsKey, recommendationsKey).catch(() => undefined)
      }
      break
    }
    case 'lines':
      useWorkStore.setState((state) => {
        const log = [...state.log, ...event.lines]
        return { log: log.length > maxLines ? log.slice(-maxLines) : log }
      })
      break
    case 'cleared':
      useWorkStore.setState({ log: [] })
      break
    case 'work_changed':
      void queryClient.invalidateQueries({ queryKey: workKey(event.project) })
      void queryClient.invalidateQueries({ queryKey: ['review-notes'] })
      break
    case 'finished':
      if (event.outcome === 'created') {
        void refresh(projectListKey).catch(() => undefined)
        useWorkStore.setState({ project: event.project })
      } else {
        // What the step left is shown when it is about the project on screen.
        const store = useWorkStore.getState()
        if (store.project === event.project) {
          store.setTab(event.outcome === 'proposals' ? 'glossary' : 'corrections')
        }
      }
      break
  }
}

/** Binds the queue's events; called once at startup. */
export async function subscribeQueue(active: () => boolean): Promise<void> {
  const channel = new Channel<QueueEvent>((event) => {
    if (active()) receive(event)
  })
  const snapshot = await call(commands.subscribeQueue, channel)
  useWorkStore.setState({ queue: snapshot.state, log: snapshot.log })
}
