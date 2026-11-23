import { beforeEach, expect, test } from 'bun:test'
import { assistantUi } from './assistantStore'

beforeEach(() => {
  window.localStorage.removeItem('orbit:assistant_follow')
  window.localStorage.removeItem('orbit:assistant_wide')
  window.sessionStorage.removeItem('orbit:assistant_open')
  assistantUi.reset()
})

test('toggle opens and closes, and every open asks the panel to focus its input', () => {
  assistantUi.toggle()
  expect(assistantUi.get().open).toBe(true)
  const token = assistantUi.get().focusToken
  assistantUi.toggle()
  expect(assistantUi.get().open).toBe(false)
  assistantUi.open()
  expect(assistantUi.get().focusToken).toBe(token + 1)
})

test('Follow along is on by default and remembered per browser', () => {
  expect(assistantUi.get().follow).toBe(true)
  assistantUi.setFollow(false)
  expect(window.localStorage.getItem('orbit:assistant_follow')).toBe('false')
  assistantUi.reset()
  expect(assistantUi.get().follow).toBe(false)
})

test('a reload keeps the panel open in that tab, and the wide column per browser', () => {
  assistantUi.open()
  assistantUi.setWide(true)
  assistantUi.reset()
  expect(assistantUi.get().open).toBe(true)
  expect(assistantUi.get().wide).toBe(true)
  assistantUi.close()
  assistantUi.reset()
  expect(assistantUi.get().open).toBe(false)
})
