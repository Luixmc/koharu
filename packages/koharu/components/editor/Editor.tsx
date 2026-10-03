'use client'

import { ColorSamplingProvider } from '@/components/controls/ColorSampling'
import { ActivityCenter } from '@/components/editor/ActivityCenter'
import { CanvasWorkspace } from '@/components/editor/CanvasWorkspace'
import { PageRail } from '@/components/editor/PageRail'
import { PrivacyCurtain } from '@/components/editor/PrivacyCurtain'
import { RightSidebar } from '@/components/editor/RightSidebar'
import { TextView } from '@/components/editor/TextView'
import { useKoharuStore } from '@/lib/store'
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from '@koharu/ui/components/resizable'

export function Editor() {
  const centerView = useKoharuStore((state) => state.centerView)

  return (
    <ColorSamplingProvider>
      <div className='relative min-h-0 flex-1 bg-transparent'>
        <ActivityCenter />
        <ResizablePanelGroup id='editor' orientation='horizontal' className='h-full min-h-0'>
          <ResizablePanel
            id='pages'
            defaultSize='18%'
            minSize='14%'
            maxSize='22%'
            collapsible
            collapsedSize='4%'
            className='min-h-0 overflow-hidden'
          >
            <PageRail />
          </ResizablePanel>
          <ResizableHandle className='w-0 bg-transparent' />
          <ResizablePanel
            id='canvas'
            defaultSize='58%'
            minSize='50%'
            className='workspace-corner-mask relative z-10 min-h-0 overflow-hidden rounded-tl-2xl bg-transparent shadow-[var(--shadow-content)]'
          >
            <CanvasWorkspace />
            <PrivacyCurtain />
            {centerView === 'text' && (
              // Above the curtain: text is what stays visible with the images retracted.
              <div className='absolute inset-0 z-[25]'>
                <TextView />
              </div>
            )}
          </ResizablePanel>
          <ResizableHandle className='w-0 bg-transparent' />
          <ResizablePanel
            id='inspector'
            defaultSize='24%'
            minSize='20%'
            maxSize='27%'
            collapsible
            collapsedSize='4%'
            className='min-h-0 overflow-hidden border-l border-border/40 bg-[var(--surface-panel)]'
          >
            <RightSidebar />
          </ResizablePanel>
        </ResizablePanelGroup>
      </div>
    </ColorSamplingProvider>
  )
}
