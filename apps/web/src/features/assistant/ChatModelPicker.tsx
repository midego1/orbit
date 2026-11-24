// The chat's model in the Ask AI panel's header. The list comes from the server (the models the workspace allows for a
// chat); the server checks every pick again.
import { ArrowDown2 } from 'reicon-react'
import { cn } from 'cn'
import { Tip } from '@/components/common/Tip'
import { Button } from '@/components/ui/button'
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuLabel, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { modelName } from './api'
import type { ChatModels } from './AssistantComposer'

const LABEL = 'inline-flex h-7 max-w-28 items-center gap-0.5 truncate rounded-md px-1.5 text-xs text-muted-foreground'

/** The chat's model, in the header: a menu of the allowed models, or a plain label when only one is allowed. */
export function ChatModelPicker({ chatModels, disabled }: { chatModels: ChatModels; disabled: boolean }) {
  const name = modelName(chatModels.model)
  if (chatModels.models.length < 2) {
    return (
      <Tip label={`This chat uses ${name}. A workspace owner or admin can allow other models in Settings → AI.`}>
        <Button variant="ghost" size="sm" className={cn(LABEL, 'cursor-default hover:bg-transparent')} aria-label={`Model: ${name}`} data-testid="ask-ai-model">
          {name}
        </Button>
      </Tip>
    )
  }
  return (
    <DropdownMenu>
      <Tip label={disabled ? 'The model can change when Ask AI is done' : 'The model of this chat (also /model)'}>
        <DropdownMenuTrigger
          disabled={disabled}
          render={
            <Button variant="ghost" size="sm" className={cn(LABEL, 'hover:text-foreground')} aria-label={`Model: ${name}`} data-testid="ask-ai-model">
              <span className="truncate">{name}</span>
              <ArrowDown2 className="size-3 shrink-0" aria-hidden="true" />
            </Button>
          }
        />
      </Tip>
      <DropdownMenuContent align="end" className="min-w-44">
        <DropdownMenuGroup>
          <DropdownMenuLabel>Model of this chat</DropdownMenuLabel>
          <DropdownMenuRadioGroup aria-label="Model of this chat" value={chatModels.model} onValueChange={(value) => chatModels.onPick(value as string)}>
            {chatModels.models.map((model) => (
              <DropdownMenuRadioItem key={model} value={model} closeOnClick>
                {modelName(model)}
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
