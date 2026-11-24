// Ask AI's working visuals are CSS only (index.css, the `--assistant-*` block). These checks keep the promises of that
// block: nothing moves for a user who asked for less motion, the timings stay tunable in one place, and only cheap
// properties are animated.
import { expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'

const css = readFileSync(new URL('../../index.css', import.meta.url), 'utf8')
const block = css.slice(css.indexOf('@property --assistant-shimmer-at'))

/** The block with every `@media (prefers-reduced-motion: no-preference) { … }` removed, and those bodies. */
function splitMotion(source: string): { outside: string; inside: string[] } {
  const marker = '@media (prefers-reduced-motion: no-preference)'
  let outside = ''
  const inside: string[] = []
  let at = 0
  for (let start = source.indexOf(marker); start !== -1; start = source.indexOf(marker, at)) {
    outside += source.slice(at, start)
    let depth = 0
    let end = source.indexOf('{', start)
    for (; end < source.length; end++) {
      if (source[end] === '{') depth++
      else if (source[end] === '}' && --depth === 0) break
    }
    inside.push(source.slice(start, end + 1))
    at = end + 1
  }
  return { outside: outside + source.slice(at), inside }
}

const { outside, inside } = splitMotion(block)
const animations = (source: string) => [...source.matchAll(/(?<![-\w])animation:\s*([^;]+);/g)].map((match) => match[1].trim())

test('the block is found and holds the working visuals', () => {
  expect(block.length).toBeGreaterThan(1000)
  for (const selector of ['[data-assistant-edge-layer]', '[data-assistant-bar]', '[data-assistant-status-icon]', '[data-assistant-step=\'running\']']) {
    expect(block).toContain(selector)
  }
})

test('every animation runs only when the user has not asked for less motion', () => {
  expect(animations(outside)).toEqual([])
  expect(inside.flatMap(animations).length).toBeGreaterThanOrEqual(8)
})

test('with reduced motion the edge is an even line in the tone, not a frozen light', () => {
  const reduce = block.slice(block.indexOf('@media (prefers-reduced-motion: reduce)'))
  expect(reduce).toMatch(/\[data-assistant-edge-layer\]\s*\{\s*background: var\(--assistant-tone\);/)
})

test('every timing is an --assistant-* token, and every animation has its keyframes', () => {
  for (const animation of inside.flatMap(animations)) {
    const [name, duration] = animation.split(/\s+/)
    expect(duration).toMatch(/^var\(--assistant-[\w-]+\)$/)
    expect(block).toContain(`@keyframes ${name} {`)
    expect(css).toMatch(new RegExp(`--${duration.slice(6, -1)}:\\s*[\\d.]+m?s;`))
  }
})

test('keyframes animate only transform, opacity or the registered --assistant-* properties', () => {
  for (const match of block.matchAll(/@keyframes [\w-]+ \{([\s\S]*?)\n\}/g)) {
    const properties = [...match[1].matchAll(/([\w-]+):/g)].map((property) => property[1])
    for (const property of properties) expect(['transform', 'opacity', '--assistant-shimmer-at']).toContain(property)
  }
  expect(block).toContain("@property --assistant-shimmer-at {\n  syntax: '<percentage>';")
})

test('the edge light is a rotating layer under a fixed mask, and the text is clipped only where that is supported', () => {
  expect(block).not.toContain('--assistant-edge-angle')
  expect(block).toMatch(/\[data-assistant-edge-layer='thinking'\]::before,\s*\[data-assistant-edge-layer='changing'\]::before \{[^}]*will-change: transform;/)
  expect(block).toMatch(/@supports \(\(-webkit-background-clip: text\) or \(background-clip: text\)\) \{[^@]*color: transparent;/)
  expect(block.match(/color: transparent;/g)?.length).toBe(1)
})

test('the overlays never take clicks', () => {
  for (const selector of ['[data-assistant-edge] {', '[data-assistant-dim] {', '[data-assistant-bar] {']) {
    const rule = block.slice(block.indexOf(selector), block.indexOf('}', block.indexOf(selector)))
    expect(rule).toContain('pointer-events: none;')
  }
})
