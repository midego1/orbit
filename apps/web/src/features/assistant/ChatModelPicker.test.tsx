import { expect, mock, test } from 'bun:test'
import { render, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { ChatModelPicker } from './ChatModelPicker'

test('with one allowed model the header only names it', () => {
  const view = render(<ChatModelPicker chatModels={{ model: 'claude-sonnet-5-5', models: ['claude-sonnet-5-5'], onPick: () => {} }} disabled={false} />)
  const label = view.getByTestId('ask-ai-model')
  expect(label.textContent).toBe('Sonnet 5.5')
  expect(label.getAttribute('aria-haspopup')).toBeNull()
})

test('the header menu lists only the allowed models and picks one', async () => {
  const onPick = mock((_model: string) => {})
  const view = render(<ChatModelPicker chatModels={{ model: 'claude-sonnet-5-5', models: ['claude-sonnet-5-5', 'claude-opus-5-5'], onPick }} disabled={false} />)
  await userEvent.click(view.getByRole('button', { name: 'Model: Sonnet 5.5' }))
  const items = await view.findAllByRole('menuitemradio')
  expect(items.map((item) => item.textContent)).toEqual(['Sonnet 5.5', 'Opus 5.5'])
  expect(items[0]!.getAttribute('aria-checked')).toBe('true')
  await userEvent.click(items[1]!)
  await waitFor(() => expect(onPick).toHaveBeenCalledWith('claude-opus-5-5'))
})
