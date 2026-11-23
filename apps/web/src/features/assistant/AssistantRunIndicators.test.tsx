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

test('the edge follows the state, pulses only while thinking, and the title prefix comes and goes', () => {
  reduceMotion(false)
  const view = render(<AssistantStateEdge />)
  const edge = view.container.querySelector('[data-assistant-edge]')!
  const dim = view.container.querySelector('[data-assistant-dim]')!
  expect(edge.getAttribute('data-assistant-state')).toBe('idle')
  expect(edge.getAttribute('aria-hidden')).toBe('true')
  act(() => assistantUi.setRun({ state: 'thinking', text: 'Thinking…' }))
  expect(edge.getAttribute('data-motion')).toBe('pulse')
  expect(document.title).toBe('Orbit')
  act(() => assistantUi.setRun({ state: 'waiting', text: 'Waiting for your approval' }))
  expect(dim.getAttribute('data-assistant-state')).toBe('waiting')
  expect(edge.getAttribute('data-motion')).toBe('steady')
  expect(document.title).toBe(`${TITLE_PREFIX}Orbit`)
  act(() => assistantUi.setRun({ state: 'changing', text: 'Changing GEN-3…' }))
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
