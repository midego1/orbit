// How the whole app shows what Ask AI does (assistantRunState.ts): a thin coloured edge around the window, a slight
// dim of the page while a change waits for approval, the tab title prefix, and the status line at the top of the
// panel. The edge and the dim never take clicks, so the page stays usable. Colours: `--assistant-*` in index.css.
import { useEffect, useSyncExternalStore } from 'react'
import { Edit2, Hand, SearchNormal } from 'reicon-react'
import { useAssistantUi } from './assistantStore'
import { edgeMotion, syncDocumentTitle, type AssistantRunState } from './assistantRunState'

const REDUCED_MOTION = '(prefers-reduced-motion: reduce)'
const subscribeMotion = (onChange: () => void) => {
  const media = globalThis.matchMedia?.(REDUCED_MOTION)
  media?.addEventListener('change', onChange)
  return () => media?.removeEventListener('change', onChange)
}
const reducedMotion = () => globalThis.matchMedia?.(REDUCED_MOTION).matches ?? false

function useReducedMotion(): boolean {
  return useSyncExternalStore(subscribeMotion, reducedMotion, () => false)
}

/** The window's edge and dim, and the tab title prefix. Mounted once, with the dock. */
export function AssistantStateEdge() {
  const { run } = useAssistantUi()
  const reduced = useReducedMotion()
  useEffect(() => {
    syncDocumentTitle(run.state)
  }, [run.state])
  // the prefix goes when Ask AI unmounts (sign-out, a guest page)
  useEffect(() => () => syncDocumentTitle('idle'), [])
  return (
    <>
      <div aria-hidden="true" data-assistant-dim data-assistant-state={run.state} />
      <div aria-hidden="true" data-assistant-edge data-assistant-state={run.state} data-motion={edgeMotion(run.state, reduced)} />
    </>
  )
}

function StateIcon({ state }: { state: AssistantRunState }) {
  const className = 'size-3.5 shrink-0 text-(--assistant-tone)'
  if (state === 'waiting') return <Hand className={className} aria-hidden="true" />
  if (state === 'changing') return <Edit2 className={className} aria-hidden="true" />
  return <SearchNormal className={className} aria-hidden="true" />
}

/** The line under the panel's header: icon and text, never colour alone. Always mounted, so screen readers hear each
    change (an empty live region that appears together with its text is often not read). */
export function AssistantStatusLine() {
  const { run } = useAssistantUi()
  const active = run.state !== 'idle'
  return (
    <div data-assistant-status data-assistant-state={run.state} role="status" aria-live="polite" aria-atomic="true" className={active ? 'flex shrink-0 items-center gap-2 border-b px-4 py-1.5 text-xs text-foreground' : 'sr-only'}>
      {active ? (
        <>
          <StateIcon state={run.state} />
          <span className="min-w-0 truncate">{run.text}</span>
        </>
      ) : null}
    </div>
  )
}
