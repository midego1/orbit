import { describe, expect, test } from 'bun:test'

import { formatCost, keyLine, parseTokenCap } from './ai-settings-api'

describe('Ask AI settings', () => {
  test('the key line never shows more than the last four characters', () => {
    expect(keyLine({ set: true, last4: 'AbCd', source: 'workspace' })).toBe('A key ending in …AbCd is set.')
    expect(keyLine({ set: true, last4: null, source: 'server' })).toContain('ORBIT_AI_ANTHROPIC_KEY')
    expect(keyLine({ set: false, last4: null, source: null })).toContain('No key is set')
  })

  test('the daily limit takes separators and stays within the maximum', () => {
    expect(parseTokenCap('2,000,000', 2_000_000)).toBe(2_000_000)
    expect(parseTokenCap('1 000', 2_000_000)).toBe(1000)
    expect(parseTokenCap('0', 2_000_000)).toBeNull()
    expect(parseTokenCap('3000000', 2_000_000)).toBeNull()
    expect(parseTokenCap('lots', 2_000_000)).toBeNull()
  })

  test('costs under a cent are not shown as zero', () => {
    expect(formatCost(0)).toBe('$0.00')
    expect(formatCost(0.004)).toBe('< $0.01')
    expect(formatCost(1.234)).toBe('$1.23')
  })
})
