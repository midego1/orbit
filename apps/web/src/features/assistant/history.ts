// The history's titles and its search (AssistantHistory.tsx).
import type { ConversationSummary } from './api'

export const UNTITLED = 'Untitled chat'

export function chatTitle(conversation: { title: string | null }): string {
  return conversation.title?.trim() || UNTITLED
}

/** The conversations whose title holds every word of `filter` (any case). */
export function filterConversations(conversations: ConversationSummary[], filter: string): ConversationSummary[] {
  const words = filter.toLocaleLowerCase().split(/\s+/).filter(Boolean)
  if (!words.length) return conversations
  return conversations.filter((conversation) => {
    const title = chatTitle(conversation).toLocaleLowerCase()
    return words.every((word) => title.includes(word))
  })
}
