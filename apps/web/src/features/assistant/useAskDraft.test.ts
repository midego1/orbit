import { expect, mock, test } from 'bun:test'
import { act, renderHook } from '@testing-library/react'
import { useAskDraft } from './useAskDraft'
import type { Mention } from './composer'

const mention: Mention = { kind: 'task', id: 't1', label: 'ENG-1' }

test('a sent suggestion clears what was typed and its mentions', () => {
  const send = mock((_text: string, _mentions: Mention[]) => true)
  const { result } = renderHook(() => useAskDraft(send, () => {}))
  act(() => {
    result.current.setDraft('half typed @ENG-1')
    result.current.setMentions([mention])
  })
  act(() => result.current.suggest({ label: 'What is overdue?', prompt: 'Which open tasks are overdue?' }))
  expect(send).toHaveBeenCalledWith('Which open tasks are overdue?', [])
  expect(result.current.draft).toBe('')
  expect(result.current.mentions).toEqual([])
})

test('a suggestion that could not go keeps the draft', () => {
  const { result } = renderHook(() => useAskDraft(() => false, () => {}))
  act(() => result.current.setDraft('mine'))
  act(() => result.current.suggest({ label: 'x', prompt: 'Summarize.' }))
  expect(result.current.draft).toBe('mine')
})

test('an open suggestion fills the field and puts the cursor at its end', () => {
  const send = mock(() => true)
  const focus = mock((_at: number) => {})
  const { result } = renderHook(() => useAskDraft(send, focus))
  act(() => result.current.suggest({ label: 'Create a task', prompt: 'Create a task: ' }))
  expect(send).not.toHaveBeenCalled()
  expect(result.current.draft).toBe('Create a task: ')
  expect(focus).toHaveBeenCalledWith(15)
})
