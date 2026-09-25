// Value formatting and clipboard helpers.
import type { CellValue, ColumnMeta } from '@shared/types'
import { isHexBinary } from '@shared/sql'

export function isNumericType(type?: string): boolean {
  return !!type && /^(int|integer|smallint|bigint|tinyint|mediumint|int2|int4|int8|serial|bigserial|numeric|decimal|number|real|float|double|money|oid|year|dec)/i.test(type)
}

export function displayValue(v: CellValue, maxLen = 300): string {
  if (v === null) return 'NULL'
  if (typeof v === 'boolean') return v ? 'true' : 'false'
  if (typeof v === 'number') return String(v)
  const str: string = v
  if (isHexBinary(str)) {
    const bytes = (str.length - 2) / 2
    return bytes <= 16 ? `0x${str.slice(2).toUpperCase()}` : `[binary ${formatBytes(bytes)}]`
  }
  const s = str.length > maxLen ? str.slice(0, maxLen) + '…' : str
  return s.replace(/[\r\n\t]+/g, ' ')
}

export function rawValue(v: CellValue): string {
  if (v === null) return ''
  return String(v)
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${(n / 1024 / 1024).toFixed(1)} MB`
}

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms} ms`
  if (ms < 60000) return `${(ms / 1000).toFixed(2)} s`
  return `${Math.floor(ms / 60000)}m ${Math.round((ms % 60000) / 1000)}s`
}

export function formatCount(n: number): string {
  return n.toLocaleString()
}

function csvCell(v: CellValue, delim: string): string {
  if (v === null) return ''
  const s = String(v)
  return s.includes(delim) || s.includes('"') || s.includes('\n') || s.includes('\r') ? `"${s.replace(/"/g, '""')}"` : s
}

export function toDelimited(columns: string[] | null, rows: CellValue[][], delim: string): string {
  const lines: string[] = []
  if (columns) lines.push(columns.map((c) => csvCell(c, delim)).join(delim))
  for (const r of rows) lines.push(r.map((v) => (delim === '\t' ? rawValue(v).replace(/[\t\n\r]/g, ' ') : csvCell(v, delim))).join(delim))
  return lines.join('\n')
}

export function toJson(columns: ColumnMeta[], rows: CellValue[][]): string {
  return JSON.stringify(
    rows.map((r) => Object.fromEntries(columns.map((c, i) => [c.name, r[i]]))),
    null,
    2
  )
}

export function toMarkdown(columns: string[], rows: CellValue[][]): string {
  const esc = (s: string) => s.replace(/\|/g, '\\|').replace(/\n/g, ' ')
  const out = [`| ${columns.map(esc).join(' | ')} |`, `|${columns.map(() => ' --- ').join('|')}|`]
  for (const r of rows) out.push(`| ${r.map((v) => (v === null ? 'NULL' : esc(String(v)))).join(' | ')} |`)
  return out.join('\n')
}

export async function copyText(text: string): Promise<void> {
  await navigator.clipboard.writeText(text)
}

export function prettyValue(v: CellValue): string {
  if (v === null) return 'NULL'
  const s = String(v)
  const t = s.trim()
  if ((t.startsWith('{') && t.endsWith('}')) || (t.startsWith('[') && t.endsWith(']'))) {
    try {
      return JSON.stringify(JSON.parse(t), null, 2)
    } catch {
      /* not JSON */
    }
  }
  return s
}
