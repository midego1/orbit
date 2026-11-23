// Where the Ask AI panel lives. On a desktop it is a column at the right of the content (400px, or half the window
// when widened) that stays mounted once opened, so closing it does not cut off an answer; on a phone it is a sheet
// from the right. Ctrl+J (Cmd+J) opens and closes it everywhere in the workspace.
import { lazy, Suspense, useState, useSyncExternalStore } from 'react'
import { cn } from 'cn'
import { ErrorBoundary } from '@/components/common/ErrorBoundary'
import { SideSheet, SideSheetContent } from '@/components/common/SideSheet'
import { Spinner } from '@/components/ui/spinner'
import { useCommand } from '@/shortcuts/useCommand'
import { useWorkspace } from '@/features/workspaces/workspaceContext'
import { assistantUi, useAssistantUi } from './assistantStore'

// its own chunk: the AG-UI runtime is most of its size
const AssistantPanel = lazy(() => import('./AssistantPanel'))

const desktopQuery = '(min-width: 900px)'
const subscribeDesktop = (onChange: () => void) => {
  const media = window.matchMedia(desktopQuery)
  media.addEventListener('change', onChange)
  return () => media.removeEventListener('change', onChange)
}
const isDesktop = () => window.matchMedia(desktopQuery).matches

function PanelLoader() {
  const { workspace } = useWorkspace()
  const close = () => assistantUi.close()
  return (
    <ErrorBoundary resetKey={workspace.id}>
      <Suspense
        fallback={
          <div className="flex h-full items-center justify-center">
            <Spinner className="text-muted-foreground" />
          </div>
        }
      >
        <AssistantPanel key={workspace.id} onClose={close} />
      </Suspense>
    </ErrorBoundary>
  )
}

export function AssistantDock() {
  const { open, wide } = useAssistantUi()
  const desktop = useSyncExternalStore(subscribeDesktop, isDesktop)
  // mounted from the first open on, so a closed column keeps its running answer
  const [opened, setOpened] = useState(open)
  if (open && !opened) setOpened(true)
  useCommand('assistant.toggle', () => assistantUi.toggle())

  if (desktop) {
    if (!opened) return null
    // closed, the column is hidden but stays mounted (display: none adds no gap in the floating layout)
    return (
      <aside
        data-assistant
        className={cn(
          'flex min-h-0 shrink-0 overflow-hidden border-l bg-background min-[900px]:floating:rounded-lg min-[900px]:floating:border',
          !open ? 'hidden' : wide ? 'w-1/2 min-w-[400px]' : 'w-[400px]',
        )}
      >
        <PanelLoader />
      </aside>
    )
  }
  return (
    <SideSheet open={open} onOpenChange={(next) => (next ? assistantUi.open() : assistantUi.close())}>
      <SideSheetContent side="right" data-assistant aria-label="Ask AI" className="z-[41] w-full overflow-hidden border-l bg-background pt-[env(safe-area-inset-top,0px)] pb-[env(safe-area-inset-bottom,0px)] sm:max-w-[420px]">
        <PanelLoader />
      </SideSheetContent>
    </SideSheet>
  )
}
