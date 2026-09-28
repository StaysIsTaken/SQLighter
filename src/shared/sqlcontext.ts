// Lightweight SQL context analysis for the editor: which tables a statement uses (with aliases)
// and whether the cursor is at a place where column names make sense. Tolerates incomplete SQL.

export interface TableRef {
  schema?: string
  name: string
  alias?: string
}

interface Token {
  text: string
  /** Upper-cased keyword/word, or '' for punctuation and quoted identifiers. */
  word: string
  kind: 'word' | 'ident' | 'punct' | 'string'
  start: number
  end: number
}

/** Words that end a table reference (so they are never taken as alias). */
const NOT_ALIAS = new Set(
  'WHERE JOIN INNER LEFT RIGHT FULL OUTER CROSS NATURAL ON USING GROUP ORDER HAVING LIMIT OFFSET UNION EXCEPT INTERSECT MINUS SET VALUES SELECT WINDOW FETCH FOR RETURNING WITH AND OR AS TOP STRAIGHT_JOIN LATERAL APPLY PIVOT UNPIVOT TABLESAMPLE PARTITION DEFAULT OUTPUT'.split(' ')
)
/** Keywords after which a table name is expected. */
const TABLE_KW = new Set(['FROM', 'JOIN', 'UPDATE', 'INTO', 'TABLE', 'STRAIGHT_JOIN', 'APPLY'])
/** Keywords that start a part of the statement where column names are expected. */
const COLUMN_KW = new Set(['SELECT', 'WHERE', 'ON', 'BY', 'HAVING', 'SET', 'AND', 'OR', 'NOT', 'WHEN', 'THEN', 'ELSE', 'CASE', 'DISTINCT', 'USING', 'RETURNING', 'BETWEEN', 'IN', 'LIKE', 'IS'])

function tokenize(sql: string): Token[] {
  const out: Token[] = []
  let i = 0
  const n = sql.length
  while (i < n) {
    const ch = sql[i]
    if (/\s/.test(ch)) {
      i++
      continue
    }
    if (ch === '-' && sql[i + 1] === '-') {
      while (i < n && sql[i] !== '\n') i++
      continue
    }
    if (ch === '/' && sql[i + 1] === '*') {
      const e = sql.indexOf('*/', i + 2)
      i = e < 0 ? n : e + 2
      continue
    }
    const close = ch === '"' ? '"' : ch === '`' ? '`' : ch === '[' ? ']' : ch === "'" ? "'" : null
    if (close) {
      let j = i + 1
      while (j < n) {
        if (sql[j] === close) {
          if (sql[j + 1] === close && close !== ']') {
            j += 2
            continue
          }
          break
        }
        j++
      }
      const text = sql.slice(i + 1, Math.min(j, n)).replace(close === ']' ? /]]/g : new RegExp(close + close, 'g'), close)
      out.push({ text, word: '', kind: ch === "'" ? 'string' : 'ident', start: i, end: Math.min(j + 1, n) })
      i = j + 1
      continue
    }
    const m = /^[A-Za-z_À-￿][\w$À-￿]*/.exec(sql.slice(i, i + 256))
    if (m) {
      out.push({ text: m[0], word: m[0].toUpperCase(), kind: 'word', start: i, end: i + m[0].length })
      i += m[0].length
      continue
    }
    out.push({ text: ch, word: '', kind: 'punct', start: i, end: i + 1 })
    i++
  }
  return out
}

/** The statement around `pos` (split at semicolons outside strings/comments). */
export function statementAt(doc: string, pos: number): { text: string; offset: number } {
  let start = 0
  for (const t of tokenize(doc)) {
    if (t.kind === 'punct' && t.text === ';') {
      if (t.end <= pos) start = t.end
      else return { text: doc.slice(start, t.start), offset: start }
    }
  }
  return { text: doc.slice(start), offset: start }
}

const isName = (t: Token | undefined) => !!t && (t.kind === 'ident' || (t.kind === 'word' && !NOT_ALIAS.has(t.word)))

/**
 * Tables referenced by FROM / JOIN / UPDATE / INSERT INTO, with their aliases.
 * `topLevelOnly` ignores tables inside subqueries (they do not contribute result columns).
 */
export function tableRefs(stmt: string, topLevelOnly = false): TableRef[] {
  const toks = tokenize(stmt)
  const depth: number[] = []
  let d = 0
  for (const t of toks) {
    if (t.kind === 'punct' && t.text === ')') d = Math.max(0, d - 1)
    depth.push(d)
    if (t.kind === 'punct' && t.text === '(') d++
  }
  const refs: TableRef[] = []
  const readRef = (i: number): number => {
    // name [. name [. name]]  [AS] [alias]
    if (!isName(toks[i])) return i
    const parts = [toks[i].text]
    let j = i + 1
    while (toks[j]?.text === '.' && toks[j].kind === 'punct' && isName(toks[j + 1])) {
      parts.push(toks[j + 1].text)
      j += 2
    }
    const ref: TableRef = { name: parts[parts.length - 1] }
    if (parts.length > 1) ref.schema = parts[parts.length - 2]
    if (toks[j]?.word === 'AS') j++
    if (isName(toks[j]) && toks[j + 1]?.text !== '(') {
      ref.alias = toks[j].text
      j++
    }
    refs.push(ref)
    return j
  }
  for (let i = 0; i < toks.length; i++) {
    const w = toks[i].word
    if (!TABLE_KW.has(w) || (topLevelOnly && depth[i] > 0)) continue
    // INSERT INTO t (...), DELETE FROM t, SELECT ... FROM a, b
    let j = readRef(i + 1)
    if (w === 'FROM') {
      while (toks[j]?.text === ',') j = readRef(j + 1)
    }
    i = Math.max(i, j - 1)
  }
  return refs
}

/**
 * What belongs at `pos` (relative to the statement): 'columns' in SELECT lists, WHERE, ON,
 * ORDER/GROUP BY, SET, …; 'tables' right after FROM/JOIN/INTO/UPDATE; 'member' after a dot.
 */
export function clauseAt(stmt: string, pos: number): 'columns' | 'tables' | 'member' | 'other' {
  const toks = tokenize(stmt).filter((t) => t.end <= pos || t.start < pos)
  // Ignore the word currently being typed.
  let k = toks.length - 1
  if (k >= 0 && toks[k].end >= pos && toks[k].kind === 'word') k--
  if (k >= 0 && toks[k].text === '.' && toks[k].kind === 'punct') return 'member'
  if (k >= 0 && toks[k].kind === 'string') return 'other'
  let depth = 0
  for (let i = k; i >= 0; i--) {
    const t = toks[i]
    if (t.kind === 'punct') {
      if (t.text === ')') depth++
      else if (t.text === '(') {
        if (depth === 0) {
          // INSERT INTO t ( | ) -> column list
          if (isName(toks[i - 1]) && toks.slice(0, i).some((x) => x.word === 'INTO')) {
            const before = toks[i - 2]
            if (before?.word === 'INTO' || (before?.text === '.' && toks[i - 4]?.word === 'INTO')) return 'columns'
          }
          continue
        }
        depth--
      }
      continue
    }
    if (depth > 0 || t.kind !== 'word') continue
    if (TABLE_KW.has(t.word)) {
      // "FROM users u |" or "FROM a, |": still in the table list
      return 'tables'
    }
    if (COLUMN_KW.has(t.word)) return 'columns'
  }
  return 'other'
}
