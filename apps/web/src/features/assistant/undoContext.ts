// How a summary card asks the server to undo changes Ask AI made (`POST …/conversations/{id}/undo`). The panel
// provides it for its conversation; without it a card shows no Undo buttons.
import { createContext } from 'react'
import type { UndoResult } from './api'

export type UndoChanges = (ids: string[]) => Promise<UndoResult[]>

export const UndoContext = createContext<UndoChanges | null>(null)
