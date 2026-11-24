// The Ask AI composer's draft and its mentions, and what a suggestion does with them.
import { useState } from 'react'
import { isOpenPrompt, type Mention, type Suggestion } from './composer'

/**
 * `send` posts a message and says whether it went. A suggestion that ends open ("Create a task: ") fills the field and
 * asks for the cursor at its end (`focus`); any other is sent, and then the field and its mentions are cleared, as after
 * a typed message.
 */
export function useAskDraft(send: (text: string, mentions: Mention[]) => boolean, focus: (at: number) => void) {
  const [draft, setDraft] = useState('')
  const [mentions, setMentions] = useState<Mention[]>([])
  function suggest(suggestion: Suggestion) {
    if (isOpenPrompt(suggestion.prompt)) {
      setDraft(suggestion.prompt)
      focus(suggestion.prompt.length)
      return
    }
    if (send(suggestion.prompt, [])) {
      setDraft('')
      setMentions([])
    }
  }
  return { draft, setDraft, mentions, setMentions, suggest }
}
