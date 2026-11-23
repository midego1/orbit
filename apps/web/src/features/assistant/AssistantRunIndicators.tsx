// How the whole app shows what Ask AI does (assistantRunState.ts): a light that travels around the window edge (it
// stops, violet, while a change waits for approval), a slight dim of the page while it waits, the tab title prefix,
// and the status line at the top of the panel. The edge and the dim never take clicks, so the page stays usable.
// Colours, sizes and timings: the `--assistant-*` block in index.css; all motion is CSS (no animation loop here).
import { useEffect, useState, useSyncExternalStore } from 'react'
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
    if (run.state !== 'waiting') return
    // a page sets its own title when the user moves on while the change waits: put the prefix back
    const observer = new MutationObserver(() => syncDocumentTitle('waiting'))
    observer.observe(document.head, { subtree: true, childList: true, characterData: true })
    return () => observer.disconnect()
  }, [run.state])
  // the prefix goes when Ask AI unmounts (sign-out, a guest page)
  useEffect(() => () => syncDocumentTitle('idle'), [])
  // the last state that showed, so the edge keeps its colour while it fades out
  const [tone, setTone] = useState<AssistantRunState>(run.state)
  if (run.state !== 'idle' && run.state !== tone) setTone(run.state)
  return (
    <>
      <div aria-hidden="true" data-assistant-dim data-assistant-state={run.state} />
      <div aria-hidden="true" data-assistant-edge data-assistant-state={run.state} data-edge-tone={tone} data-motion={edgeMotion(run.state, reduced)}>
        <span data-assistant-edge-layer="thinking" />
        <span data-assistant-edge-layer="changing" />
        <span data-assistant-edge-layer="waiting" />
      </div>
    </>
  )
}

/** The state's icon in a small tinted disc; index.css moves it while Ask AI works. */
function StateIcon({ state }: { state: AssistantRunState }) {
  const className = 'size-3 shrink-0'
  return (
    <span data-assistant-status-icon className="relative flex size-5 shrink-0 items-center justify-center rounded-full text-(--assistant-tone)">
      {state === 'waiting' ? <Hand className={className} aria-hidden="true" /> : state === 'changing' ? <Edit2 className={className} aria-hidden="true" /> : <SearchNormal className={className} aria-hidden="true" />}
    </span>
  )
}

/** The line under the panel's header: icon and text, never colour alone. Always mounted, so screen readers hear each
    change (an empty live region that appears together with its text is often not read). */
export function AssistantStatusLine() {
  const { run } = useAssistantUi()
  const reduced = useReducedMotion()
  const active = run.state !== 'idle'
  return (
    <div data-assistant-status data-assistant-state={run.state} data-motion={edgeMotion(run.state, reduced)} role="status" aria-live="polite" aria-atomic="true" className={active ? 'relative flex shrink-0 items-center gap-2 overflow-hidden border-b py-1.5 pr-4 pl-3.5 text-xs text-foreground' : 'sr-only'}>
      {active ? (
        <>
          <span aria-hidden="true" data-assistant-bar />
          <StateIcon state={run.state} />
          <span data-assistant-status-text className="min-w-0 truncate">{run.text}</span>
        </>
      ) : null}
    </div>
  )
}
