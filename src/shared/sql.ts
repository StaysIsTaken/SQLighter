// Dialect-aware SQL helpers: statement splitting, quoting, literals and statement classification.
import type { CellValue, Dialect } from './types'

export interface SqlStatement {
  text: string
  start: number
  end: number
}

/**
 * Splits a script into statements. Understands quotes, quoted identifiers, line/block comments,
 * PostgreSQL dollar quoting, MySQL "DELIMITER" directives and SQL Server "GO" batch separators.
 * Also treats PL/SQL / T-SQL / trigger BEGIN ... END blocks as one statement where possible.
 */
export function splitStatements(sql: string, dialect: Dialect): SqlStatement[] {
  const out: SqlStatement[] = []
  let delimiter = ';'
  let i = 0
  let stmtStart = 0
  const n = sql.length
  // Depth of BEGIN...END blocks for procedural code (oracle, sqlite triggers, mysql w/o DELIMITER).
  let blockDepth = 0
  let lastWord = ''

  const push = (end: number, next: number) => {
    const raw = sql.slice(stmtStart, end)
    const text = raw.trim()
    if (text && !isOnlyComments(text)) {
      const lead = raw.length - raw.trimStart().length
      out.push({ text, start: stmtStart + lead, end: stmtStart + lead + text.length })
    }
    stmtStart = next
    blockDepth = 0
    lastWord = ''
  }

  const atLineStart = (pos: number) => {
    let p = pos - 1
    while (p >= 0 && (sql[p] === ' ' || sql[p] === '\t')) p--
    return p < 0 || sql[p] === '\n' || sql[p] === '\r'
  }

  while (i < n) {
    const c = sql[i]
    const c2 = sql[i + 1]

    // MySQL: DELIMITER directive (client-side command)
    if (dialect === 'mysql' && (c === 'D' || c === 'd') && atLineStart(i) && /^delimiter\s/i.test(sql.slice(i, i + 10))) {
      const lineEnd = sql.indexOf('\n', i)
      const end = lineEnd === -1 ? n : lineEnd
      push(i, i)
      const d = sql.slice(i + 9, end).trim()
      if (d) delimiter = d
      i = end + 1
      stmtStart = i
      continue
    }
    // SQL Server: GO batch separator on its own line
    if (dialect === 'mssql' && (c === 'G' || c === 'g') && (c2 === 'O' || c2 === 'o') && atLineStart(i)) {
      const rest = sql.slice(i + 2)
      const m = /^[ \t]*(\r?\n|$)/.exec(rest)
      if (m) {
        push(i, i + 2 + m[0].length)
        i = i + 2 + m[0].length
        continue
      }
    }
    // Comments
    if (c === '-' && c2 === '-') {
      const e = sql.indexOf('\n', i)
      i = e === -1 ? n : e + 1
      continue
    }
    if (c === '#' && dialect === 'mysql') {
      const e = sql.indexOf('\n', i)
      i = e === -1 ? n : e + 1
      continue
    }
    if (c === '/' && c2 === '*') {
      const e = sql.indexOf('*/', i + 2)
      i = e === -1 ? n : e + 2
      continue
    }
    // Quotes
    if (c === "'" || c === '"' || (c === '`' && dialect === 'mysql')) {
      i = skipQuoted(sql, i, c, dialect === 'mysql' && c !== '`')
      continue
    }
    if (c === '[' && dialect === 'mssql') {
      const e = sql.indexOf(']', i + 1)
      i = e === -1 ? n : e + 1
      continue
    }
    if (c === '$' && dialect === 'postgres') {
      const m = /^\$([A-Za-z_][A-Za-z0-9_]*)?\$/.exec(sql.slice(i))
      if (m) {
        const tag = m[0]
        const e = sql.indexOf(tag, i + tag.length)
        i = e === -1 ? n : e + tag.length
        continue
      }
    }
    // Words (track BEGIN/END/CASE for procedural blocks)
    if (/[A-Za-z_]/.test(c)) {
      let j = i + 1
      while (j < n && /[A-Za-z0-9_$#]/.test(sql[j])) j++
      const word = sql.slice(i, j).toUpperCase()
      if (delimiter === ';' && dialect !== 'mssql' && dialect !== 'postgres') {
        if (word === 'BEGIN' || word === 'CASE') {
          // "BEGIN TRANSACTION"/"BEGIN;" is not a block
          const after = sql.slice(j).match(/^\s*(\S+)/)?.[1]?.toUpperCase() ?? ''
          const isTx = word === 'BEGIN' && (after.startsWith(';') || after === 'TRANSACTION' || after === 'WORK' || after === 'DEFERRED' || after === 'IMMEDIATE' || after === 'EXCLUSIVE' || after === '')
          if (!isTx) blockDepth++
        } else if (word === 'END' && blockDepth > 0) {
          const after = sql.slice(j).match(/^\s*([A-Za-z]+)/)?.[1]?.toUpperCase()
          if (after !== 'IF' && after !== 'LOOP' && after !== 'CASE' && after !== 'WHILE' && after !== 'REPEAT') blockDepth--
        }
        // Oracle anonymous blocks / CREATE ... AS|IS ... END; the leading keyword opens the block.
        if (dialect === 'oracle' && (word === 'DECLARE' && lastWord === '')) blockDepth++
      }
      lastWord = word
      i = j
      continue
    }
    // Oracle "/" on its own line terminates PL/SQL
    if (dialect === 'oracle' && c === '/' && atLineStart(i) && /^\/[ \t]*(\r?\n|$)/.test(sql.slice(i))) {
      push(i, i + 1)
      i++
      continue
    }
    // Delimiter
    if (sql.startsWith(delimiter, i) && blockDepth === 0) {
      push(i, i + delimiter.length)
      i += delimiter.length
      continue
    }
    i++
  }
  push(n, n)
  return out
}

function skipQuoted(sql: string, i: number, q: string, backslashEscapes: boolean): number {
  let j = i + 1
  while (j < sql.length) {
    const ch = sql[j]
    if (backslashEscapes && ch === '\\') {
      j += 2
      continue
    }
    if (ch === q) {
      if (sql[j + 1] === q) {
        j += 2
        continue
      }
      return j + 1
    }
    j++
  }
  return sql.length
}

function isOnlyComments(text: string): boolean {
  return stripComments(text).trim() === ''
}

/** Removes comments (not inside quotes). */
export function stripComments(sql: string): string {
  let out = ''
  let i = 0
  while (i < sql.length) {
    const c = sql[i]
    const c2 = sql[i + 1]
    if (c === '-' && c2 === '-') {
      const e = sql.indexOf('\n', i)
      i = e === -1 ? sql.length : e
      continue
    }
    if (c === '/' && c2 === '*') {
      const e = sql.indexOf('*/', i + 2)
      i = e === -1 ? sql.length : e + 2
      out += ' '
      continue
    }
    if (c === "'" || c === '"' || c === '`') {
      const e = skipQuoted(sql, i, c, false)
      out += sql.slice(i, e)
      i = e
      continue
    }
    out += c
    i++
  }
  return out
}

/** Finds the statement under the cursor (or the one ending right before it). */
export function statementAt(sql: string, pos: number, dialect: Dialect): SqlStatement | null {
  const stmts = splitStatements(sql, dialect)
  if (!stmts.length) return null
  for (const s of stmts) if (pos >= s.start && pos <= s.end + 1) return s
  let best: SqlStatement | null = null
  for (const s of stmts) if (s.end <= pos) best = s
  return best ?? stmts[0]
}

// ---------------------------------------------------------------------------------------------
// Quoting

export function quoteIdent(name: string, dialect: Dialect): string {
  switch (dialect) {
    case 'mysql':
      return '`' + name.replace(/`/g, '``') + '`'
    case 'mssql':
      return '[' + name.replace(/]/g, ']]') + ']'
    default:
      return '"' + name.replace(/"/g, '""') + '"'
  }
}

export function qualifiedName(schema: string | undefined | null, name: string, dialect: Dialect): string {
  if (!schema || dialect === 'sqlite' && schema === 'main') return quoteIdent(name, dialect)
  return quoteIdent(schema, dialect) + '.' + quoteIdent(name, dialect)
}

const HEX_PREFIX = '\\x'

/** Binary values are transported as "\x..." hex strings; see normalizeValue in the main process. */
export function isHexBinary(v: unknown): boolean {
  return typeof v === 'string' && v.startsWith(HEX_PREFIX) && /^\\x[0-9a-f]*$/i.test(v)
}

const NUMERIC_TYPE = /^(int|integer|smallint|bigint|tinyint|mediumint|int2|int4|int8|serial|bigserial|numeric|decimal|number|real|float|double|money|dec)\b/

export function quoteLiteral(value: CellValue | undefined, dialect: Dialect, dataType?: string): string {
  if (value === null || value === undefined) return 'NULL'
  if (typeof value === 'boolean') {
    if (dialect === 'postgres') return value ? 'TRUE' : 'FALSE'
    return value ? '1' : '0'
  }
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) return `'${String(value)}'`
    return String(value)
  }
  const type = (dataType ?? '').toLowerCase()
  if (isHexBinary(value) && (type === '' || /bytea|blob|binary|image|raw/.test(type))) {
    const hex = value.slice(2)
    switch (dialect) {
      case 'postgres':
        return `'\\x${hex}'::bytea`
      case 'mssql':
        return '0x' + hex
      case 'oracle':
        return `HEXTORAW('${hex}')`
      default:
        return `X'${hex}'`
    }
  }
  // Values edited in the grid arrive as text: keep numbers/booleans typed for their columns.
  if (NUMERIC_TYPE.test(type) && /^-?\d+(\.\d+)?([eE][-+]?\d+)?$/.test(value.trim())) return value.trim()
  if (/^(bool|boolean)$/.test(type) && /^(true|false)$/i.test(value.trim())) {
    const b = value.trim().toLowerCase() === 'true'
    return dialect === 'postgres' ? (b ? 'TRUE' : 'FALSE') : b ? '1' : '0'
  }
  let s = value.replace(/'/g, "''")
  if (dialect === 'mysql') s = s.replace(/\\/g, '\\\\')
  if (dialect === 'mssql' && /[^\x00-\x7f]/.test(s)) return `N'${s}'`
  return `'${s}'`
}

// ---------------------------------------------------------------------------------------------
// Classification

const READ_ONLY_START = /^(select|with|show|explain|describe|desc|values|table|pragma)\b/i

/** Leading keyword of a statement (comments stripped), upper-cased. */
export function leadingKeyword(stmt: string): string {
  const s = stripComments(stmt).trim()
  const m = /^[A-Za-z]+/.exec(s)
  return m ? m[0].toUpperCase() : ''
}

/**
 * Conservative check whether a statement is read-only. Used as a *first* line of defence for AI
 * agents / MCP; the real enforcement is a READ ONLY transaction on the database side.
 */
export function isReadOnlyStatement(stmt: string): boolean {
  const s = stripComments(stmt).trim().replace(/;\s*$/, '')
  if (!READ_ONLY_START.test(s)) return false
  if (/;/.test(stripStrings(s))) return false // multiple statements
  const kw = leadingKeyword(s)
  if (kw === 'PRAGMA') return !/=/.test(s) // PRAGMA x = y writes
  if (kw === 'SELECT' || kw === 'WITH' || kw === 'VALUES' || kw === 'TABLE') {
    // SELECT ... INTO creates tables (pg/mssql), CTEs may contain DML (postgres), FOR UPDATE locks rows.
    return !/\b(insert|update|delete|merge|create|alter|drop|truncate|grant|revoke|into|call|exec|execute|lock)\b/i.test(stripStrings(s))
  }
  if (kw === 'EXPLAIN') return !/\banalyze\b/i.test(s) // EXPLAIN ANALYZE executes the statement
  return true
}

function stripStrings(s: string): string {
  return s.replace(/'(?:[^']|'')*'/g, "''").replace(/"(?:[^"]|"")*"/g, '""').replace(/`[^`]*`/g, '``')
}

export interface DangerInfo {
  destructive: boolean
  modifies: boolean
  reason?: string
}

/** Detects statements that modify data and especially the destructive ones. */
export function analyzeStatement(stmt: string): DangerInfo {
  const s = stripStrings(stripComments(stmt)).trim()
  const kw = leadingKeyword(s)
  if (!kw) return { destructive: false, modifies: false }
  if (kw === 'DROP') return { destructive: true, modifies: true, reason: 'DROP' }
  if (kw === 'TRUNCATE') return { destructive: true, modifies: true, reason: 'TRUNCATE' }
  if (kw === 'DELETE' && !/\bwhere\b/i.test(s)) return { destructive: true, modifies: true, reason: 'DELETE without WHERE' }
  if (kw === 'UPDATE' && !/\bwhere\b/i.test(s)) return { destructive: true, modifies: true, reason: 'UPDATE without WHERE' }
  if (kw === 'ALTER' && /\bdrop\b/i.test(s)) return { destructive: true, modifies: true, reason: 'ALTER ... DROP' }
  if (isReadOnlyStatement(stmt)) return { destructive: false, modifies: false }
  return { destructive: false, modifies: true }
}

// ---------------------------------------------------------------------------------------------
// Paging

export function limitQuery(base: string, limit: number, offset: number, dialect: Dialect, hasOrder: boolean): string {
  switch (dialect) {
    case 'mssql':
      return `${base}${hasOrder ? '' : ' ORDER BY (SELECT NULL)'} OFFSET ${offset} ROWS FETCH NEXT ${limit} ROWS ONLY`
    case 'oracle':
      return `${base} OFFSET ${offset} ROWS FETCH NEXT ${limit} ROWS ONLY`
    default:
      return `${base} LIMIT ${limit} OFFSET ${offset}`
  }
}
