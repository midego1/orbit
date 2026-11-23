import { createContext, Fragment, type ReactNode } from 'react'
import { TaskChip } from './AssistantCards'
import { findKnownKeys, type KnownTask } from './cards'

/** Draws the known task keys of a run of an answer's plain text as chips (`MarkdownTextContext`); code is never a run. */
export function taskChipRun(known: ReadonlyMap<string, KnownTask>) {
  return function textRun(run: string, plain: (text: string) => ReactNode): ReactNode {
    const matches = findKnownKeys(run, known)
    if (matches.length === 0) return plain(run)
    const parts: ReactNode[] = []
    let cursor = 0
    for (const match of matches) {
      parts.push(
        <Fragment key={match.start}>
          {plain(run.slice(cursor, match.start))}
          <TaskChip task={match.task} text={run.slice(match.start, match.end)} />
        </Fragment>,
      )
      cursor = match.end
    }
    return [...parts, <Fragment key="rest">{plain(run.slice(cursor))}</Fragment>]
  }
}

/** The conversation's chip renderer (`taskChipRun` of its known keys), made once in the thread and read by each
    answer, so an answer does not watch every message itself. */
export const TaskChipContext = createContext<((text: string, plain: (text: string) => ReactNode) => ReactNode) | null>(null)
