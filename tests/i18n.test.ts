import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { de } from '../src/lib/i18n.de'

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((f) => {
    const p = join(dir, f)
    return statSync(p).isDirectory() ? files(p) : /\.tsx?$/.test(f) ? [p] : []
  })
}

// Extracts the first string argument of every t('…') call.
function keys(): Set<string> {
  const out = new Set<string>(['Tables', 'Views', 'Materialized views', 'Functions', 'Procedures', 'Sequences'])
  const re = /\bt\(\s*(['"])((?:\\.|(?!\1).)*)\1/gs
  for (const f of files('src')) {
    const src = readFileSync(f, 'utf8')
    for (const m of src.matchAll(re)) out.add(JSON.parse('"' + m[2].replace(/\\'/g, "'").replace(/"/g, '\\"') + '"'))
  }
  return out
}

describe('German translation', () => {
  it('covers every UI string', () => {
    const missing = [...keys()].filter((k) => !(k in de))
    expect(missing).toEqual([])
  })
  it('keeps all placeholders', () => {
    for (const [en, tr] of Object.entries(de)) {
      const ph = (s: string) => (s.match(/\{\w+\}/g) ?? []).sort().join(',')
      expect(ph(tr), en).toBe(ph(en))
    }
  })
})
