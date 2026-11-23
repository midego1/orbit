// Follow along must not pull the page away from someone who is working on it. Two signals, both checked when a page
// step arrives:
// - Typing: focus sits in a text field or an editor outside Ask AI, or an input method is composing text there. Kept
//   up to date from focus and composition events, so nothing is looked up at the moment of the step.
// - A popup elsewhere: while any Base UI dialog, drawer, menu, popover, select or combobox is open, its focus manager
//   marks everything outside it (`data-base-ui-inert`, `inert` or `aria-hidden`). The Ask AI column is outside such a
//   popup, so when the column itself is marked, the user is busy with something else and the page stays (they get
//   the "go there" link). On a phone, Ask AI is its own sheet, which is not marked.
//   Also failing safe: when a focused field is removed from the page, some browsers send no focusout, so the guard
//   holds until focus moves again.
import { useCallback, useEffect, useRef, type RefObject } from 'react'

/** Input types where a person types text (a checkbox or a button is not "typing"). */
const TEXT_TYPES = new Set(['text', 'search', 'email', 'url', 'tel', 'password', 'number', 'date', 'datetime-local', 'month', 'time', 'week'])

export function isTextEntry(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  if (target.isContentEditable) return true
  if (target instanceof HTMLTextAreaElement) return !target.disabled && !target.readOnly
  if (target instanceof HTMLSelectElement) return !target.disabled
  if (target instanceof HTMLInputElement) return !target.disabled && !target.readOnly && TEXT_TYPES.has(target.type)
  return false
}

/** Whether `element` sits under a node that an open dialog or popup has marked inert. */
export function shutOutByModal(element: Element | null): boolean {
  return !!element?.closest('[data-base-ui-inert], [inert], [aria-hidden="true"]')
}

/** Returns a check: true while following along would interrupt the user. `panel` is Ask AI's own root element. */
export function useFollowGuard(panel: RefObject<HTMLElement | null>): () => boolean {
  const typing = useRef(false)
  const composing = useRef(false)
  useEffect(() => {
    const outsidePanel = (target: EventTarget | null) => !(target instanceof Node && panel.current?.contains(target))
    const onFocusIn = (event: FocusEvent) => {
      typing.current = isTextEntry(event.target) && outsidePanel(event.target)
    }
    // focus left for nothing (the window, the page body): no longer typing
    const onFocusOut = (event: FocusEvent) => {
      if (event.relatedTarget === null) typing.current = false
    }
    const onCompositionStart = (event: CompositionEvent) => {
      composing.current = outsidePanel(event.target)
    }
    const onCompositionEnd = () => {
      composing.current = false
    }
    // the field that has focus when Ask AI mounts counts too
    typing.current = isTextEntry(document.activeElement) && outsidePanel(document.activeElement)
    document.addEventListener('focusin', onFocusIn)
    document.addEventListener('focusout', onFocusOut)
    document.addEventListener('compositionstart', onCompositionStart)
    document.addEventListener('compositionend', onCompositionEnd)
    return () => {
      document.removeEventListener('focusin', onFocusIn)
      document.removeEventListener('focusout', onFocusOut)
      document.removeEventListener('compositionstart', onCompositionStart)
      document.removeEventListener('compositionend', onCompositionEnd)
    }
  }, [panel])
  return useCallback(() => typing.current || composing.current || shutOutByModal(panel.current), [panel])
}
