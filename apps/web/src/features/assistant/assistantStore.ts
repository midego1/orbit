// Ask AI's UI state, shared by the shell (the column or sheet), the sidebar row, the Ctrl+J command and the panel:
// whether it is open or widened, whether the page follows along, and what the assistant is doing. The conversation
// itself lives on the server and in the panel's AG-UI runtime, not here.
import { useSyncExternalStore } from 'react'

export type AssistantActivity = 'working' | 'waiting' | null

export type AssistantUi = {
  open: boolean
  /** The desktop column takes half the window instead of 400px (saved per browser). */
  wide: boolean
  /** The page moves to what Ask AI works on (saved per browser). */
  follow: boolean
  activity: AssistantActivity
  /** Bumped on every open, so the panel focuses its input again. */
  focusToken: number
}

const FOLLOW_KEY = 'orbit:assistant_follow'
const WIDE_KEY = 'orbit:assistant_wide'
/** Per tab: a reload keeps the panel (and a waiting approval card) in view; a new tab starts closed. */
const OPEN_KEY = 'orbit:assistant_open'

function read(storage: 'localStorage' | 'sessionStorage', key: string): string | null {
  try {
    return globalThis[storage]?.getItem(key) ?? null
  } catch {
    return null
  }
}

function write(storage: 'localStorage' | 'sessionStorage', key: string, value: boolean) {
  try {
    globalThis[storage]?.setItem(key, String(value))
  } catch {
    // private mode: the choice lasts for this page
  }
}

function initial(): AssistantUi {
  return {
    open: read('sessionStorage', OPEN_KEY) === 'true',
    wide: read('localStorage', WIDE_KEY) === 'true',
    follow: read('localStorage', FOLLOW_KEY) !== 'false',
    activity: null,
    focusToken: 0,
  }
}

let state: AssistantUi = initial()
const listeners = new Set<() => void>()

function update(patch: Partial<AssistantUi>) {
  const next = { ...state, ...patch }
  if ((Object.keys(patch) as (keyof AssistantUi)[]).every((key) => next[key] === state[key])) return
  state = next
  if ('open' in patch) write('sessionStorage', OPEN_KEY, next.open)
  if ('wide' in patch) write('localStorage', WIDE_KEY, next.wide)
  if ('follow' in patch) write('localStorage', FOLLOW_KEY, next.follow)
  for (const listener of listeners) listener()
}

function subscribe(listener: () => void) {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

export function useAssistantUi(): AssistantUi {
  return useSyncExternalStore(subscribe, () => state, () => state)
}

export const assistantUi = {
  get: () => state,
  open: () => update({ open: true, focusToken: state.focusToken + 1 }),
  close: () => update({ open: false }),
  toggle: () => (state.open ? update({ open: false }) : update({ open: true, focusToken: state.focusToken + 1 })),
  setWide: (wide: boolean) => update({ wide }),
  setFollow: (follow: boolean) => update({ follow }),
  setActivity: (activity: AssistantActivity) => update({ activity }),
  /** Tests only. */
  reset: () => {
    state = initial()
  },
}
