import { afterEach, describe, expect, test } from 'bun:test'
import { renderHook } from '@testing-library/react'
import { isTextEntry, shutOutByModal, useFollowGuard } from './followGuard'

afterEach(() => {
  document.body.innerHTML = ''
})

function element<K extends keyof HTMLElementTagNameMap>(tag: K, attributes: Record<string, string> = {}, parent: HTMLElement = document.body): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag)
  for (const [name, value] of Object.entries(attributes)) node.setAttribute(name, value)
  parent.appendChild(node)
  return node
}

describe('isTextEntry', () => {
  test('text fields, text areas, selects and editors count; buttons and checkboxes do not', () => {
    expect(isTextEntry(element('input'))).toBe(true)
    expect(isTextEntry(element('input', { type: 'search' }))).toBe(true)
    expect(isTextEntry(element('textarea'))).toBe(true)
    expect(isTextEntry(element('select'))).toBe(true)
    expect(isTextEntry(element('input', { type: 'checkbox' }))).toBe(false)
    expect(isTextEntry(element('input', { readonly: '' }))).toBe(false)
    expect(isTextEntry(element('button'))).toBe(false)
    expect(isTextEntry(null)).toBe(false)
  })
})

describe('shutOutByModal', () => {
  test('an element under a node an open modal marked is shut out', () => {
    const page = element('div', { 'data-base-ui-inert': '' })
    expect(shutOutByModal(element('section', {}, page))).toBe(true)
    expect(shutOutByModal(element('section'))).toBe(false)
    expect(shutOutByModal(null)).toBe(false)
  })
})

describe('useFollowGuard', () => {
  test('typing outside Ask AI holds the page; typing in Ask AI or leaving the field does not', () => {
    const panel = element('section')
    const own = element('textarea', {}, panel)
    const field = element('input')
    const { result } = renderHook(() => useFollowGuard({ current: panel }))
    expect(result.current()).toBe(false)
    field.focus()
    expect(result.current()).toBe(true)
    own.focus()
    expect(result.current()).toBe(false)
    field.focus()
    field.blur()
    expect(result.current()).toBe(false)
  })

  test('a dialog or popup elsewhere holds the page', () => {
    const shell = element('div')
    const panel = element('section', {}, shell)
    const { result } = renderHook(() => useFollowGuard({ current: panel }))
    expect(result.current()).toBe(false)
    shell.setAttribute('aria-hidden', 'true')
    expect(result.current()).toBe(true)
  })
})
