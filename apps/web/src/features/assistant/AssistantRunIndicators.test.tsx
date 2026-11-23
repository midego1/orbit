import { afterEach, beforeEach, expect, test } from 'bun:test'
import { act, render } from '@testing-library/react'
import { assistantUi } from './assistantStore'
import { AssistantStateEdge, AssistantStatusLine } from './AssistantRunIndicators'
import { IDLE_RUN, TITLE_PREFIX } from './assistantRunState'

const originalMatchMedia = globalThis.matchMedia

function reduceMotion(reduce: boolean) {
  globalThis.matchMedia = ((query: string) => ({
    matches: reduce && query.includes('prefers-reduced-motion: reduce'),
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  })) as unknown as typeof globalThis.matchMedia
}

beforeEach(() => {
  assistantUi.reset()
  document.title = 'Orbit'
})
afterEach(() => {
  globalThis.matchMedia = originalMatchMedia
  act(() => assistantUi.setRun(IDLE_RUN))
})

test('the status line says what Ask AI does, with an icon, in a polite live region', () => {
  const view = render(<AssistantStatusLine />)
  const line = view.getByRole('status')
  expect(line.getAttribute('aria-live')).toBe('polite')
  expect(line.textContent).toBe('')
  act(() => assistantUi.setRun({ state: 'thinking', text: 'Reading your projects…' }))
  expect(line.textContent).toBe('Reading your projects…')
  expect(line.getAttribute('data-assistant-state')).toBe('thinking')
  expect(line.querySelector('svg')).toBeTruthy()
  act(() => assistantUi.setRun({ state: 'waiting', text: 'Waiting for your approval' }))
  expect(view.getByRole('status')).toBe(line)
  expect(line.textContent).toBe('Waiting for your approval')
})

test('the edge follows the state, its light travels while thinking or changing, and the title prefix comes and goes', () => {
  reduceMotion(false)
  const view = render(<AssistantStateEdge />)
  const edge = view.container.querySelector('[data-assistant-edge]')!
  const dim = view.container.querySelector('[data-assistant-dim]')!
  expect(edge.getAttribute('data-assistant-state')).toBe('idle')
  expect(edge.getAttribute('aria-hidden')).toBe('true')
  act(() => assistantUi.setRun({ state: 'thinking', text: 'Thinking…' }))
  expect(edge.getAttribute('data-motion')).toBe('orbit')
  expect(document.title).toBe('Orbit')
  act(() => assistantUi.setRun({ state: 'waiting', text: 'Waiting for your approval' }))
  expect(dim.getAttribute('data-assistant-state')).toBe('waiting')
  expect(edge.getAttribute('data-motion')).toBe('steady')
  expect(document.title).toBe(`${TITLE_PREFIX}Orbit`)
  act(() => assistantUi.setRun({ state: 'changing', text: 'Changing GEN-3…' }))
  expect(edge.getAttribute('data-motion')).toBe('orbit')
  expect(document.title).toBe('Orbit')
  act(() => assistantUi.setRun({ state: 'waiting', text: 'Waiting for your approval' }))
  view.unmount()
  expect(document.title).toBe('Orbit')
})

test('with reduced motion the thinking edge does not pulse', () => {
  reduceMotion(true)
  const view = render(<AssistantStateEdge />)
  act(() => assistantUi.setRun({ state: 'thinking', text: 'Thinking…' }))
  expect(view.container.querySelector('[data-assistant-edge]')!.getAttribute('data-motion')).toBe('steady')
})

test('with reduced motion nothing on the edge or the status line moves, in any state', () => {
  reduceMotion(true)
  const view = render(
    <>
      <AssistantStateEdge />
      <AssistantStatusLine />
    </>,
  )
  for (const state of ['thinking', 'changing', 'waiting'] as const) {
    act(() => assistantUi.setRun({ state, text: 'Working…' }))
    expect(view.container.querySelector('[data-assistant-edge]')!.getAttribute('data-motion')).toBe('steady')
    expect(view.getByRole('status').getAttribute('data-motion')).toBe('steady')
  }
})

test('the edge has one layer per state and keeps the last colour while it fades out', () => {
  reduceMotion(false)
  const view = render(<AssistantStateEdge />)
  const edge = view.container.querySelector('[data-assistant-edge]')!
  const layers = [...edge.querySelectorAll('[data-assistant-edge-layer]')].map((layer) => layer.getAttribute('data-assistant-edge-layer'))
  expect(layers).toEqual(['thinking', 'changing', 'waiting'])
  expect(edge.getAttribute('data-edge-tone')).toBe('idle')
  act(() => assistantUi.setRun({ state: 'thinking', text: 'Thinking…' }))
  expect(edge.getAttribute('data-edge-tone')).toBe('thinking')
  act(() => assistantUi.setRun({ state: 'changing', text: 'Changing GEN-3…' }))
  expect(edge.getAttribute('data-edge-tone')).toBe('changing')
  // the run ends: the edge fades out in amber, not in a colour that is already gone
  act(() => assistantUi.setRun(IDLE_RUN))
  expect(edge.getAttribute('data-assistant-state')).toBe('idle')
  expect(edge.getAttribute('data-motion')).toBe('none')
  expect(edge.getAttribute('data-edge-tone')).toBe('changing')
})

test('the status line moves while Ask AI works and holds still while a change waits', () => {
  reduceMotion(false)
  const view = render(<AssistantStatusLine />)
  const line = view.getByRole('status')
  expect(line.querySelector('[data-assistant-bar]')).toBeNull()
  act(() => assistantUi.setRun({ state: 'thinking', text: 'Reading your tasks…' }))
  expect(line.getAttribute('data-motion')).toBe('orbit')
  // the light along the top is decoration only; the text and icon say what happens
  expect(line.querySelector('[data-assistant-bar]')!.getAttribute('aria-hidden')).toBe('true')
  expect(line.querySelector('[data-assistant-status-icon] svg')).toBeTruthy()
  expect(line.querySelector('[data-assistant-status-text]')!.textContent).toBe('Reading your tasks…')
  act(() => assistantUi.setRun({ state: 'changing', text: 'Changing GEN-3…' }))
  expect(line.getAttribute('data-motion')).toBe('orbit')
  act(() => assistantUi.setRun({ state: 'waiting', text: 'Waiting for your approval' }))
  expect(line.getAttribute('data-motion')).toBe('steady')
  expect(line.textContent).toBe('Waiting for your approval')
})

test('the title prefix comes back when a page sets its own title while a change waits', async () => {
  render(<AssistantStateEdge />)
  act(() => assistantUi.setRun({ state: 'waiting', text: 'Waiting for your approval' }))
  expect(document.title).toBe(`${TITLE_PREFIX}Orbit`)
  // the user opens another page, which sets its title
  document.title = 'Launch notes'
  await new Promise((resolve) => setTimeout(resolve, 0))
  expect(document.title).toBe(`${TITLE_PREFIX}Launch notes`)
  act(() => assistantUi.setRun(IDLE_RUN))
  document.title = 'Tasks'
  await new Promise((resolve) => setTimeout(resolve, 0))
  expect(document.title).toBe('Tasks')
})
