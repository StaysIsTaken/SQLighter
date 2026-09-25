// SQL generators: SELECT / INSERT / UPDATE / DELETE / UPSERT / CREATE TABLE for all dialects,
// cross-dialect type mapping and statements for saving grid edits.
import { qualifiedName, quoteIdent, quoteLiteral } from './sql'
import type { CellValue, ColumnInfo, ColumnMeta, Dialect, RowChange, TableInfo } from './types'

type Row = Record<string, CellValue>

const q = quoteIdent

export function tableName(t: Pick<TableInfo, 'schema' | 'name'>, dialect: Dialect): string {
  return qualifiedName(t.schema, t.name, dialect)
}

function typeOf(t: TableInfo, col: string): string | undefined {
  return t.columns.find((c) => c.name === col)?.dataType
}

export function generateSelect(t: TableInfo, dialect: Dialect, limit = 100): string {
  const cols = t.columns.length ? t.columns.map((c) => '  ' + q(c.name, dialect)).join(',\n') : '  *'
  const from = tableName(t, dialect)
  switch (dialect) {
    case 'mssql':
      return `SELECT TOP ${limit}\n${cols}\nFROM ${from};`
    case 'oracle':
      return `SELECT\n${cols}\nFROM ${from}\nFETCH FIRST ${limit} ROWS ONLY;`
    default:
      return `SELECT\n${cols}\nFROM ${from}\nLIMIT ${limit};`
  }
}

export function generateCount(t: TableInfo, dialect: Dialect): string {
  return `SELECT COUNT(*) FROM ${tableName(t, dialect)};`
}

export function generateInsertTemplate(t: TableInfo, dialect: Dialect): string {
  const cols = t.columns.filter((c) => !c.autoIncrement)
  const names = cols.map((c) => q(c.name, dialect)).join(', ')
  const vals = cols.map((c) => placeholder(c)).join(', ')
  return `INSERT INTO ${tableName(t, dialect)} (${names})\nVALUES (${vals});`
}

export function generateUpdateTemplate(t: TableInfo, dialect: Dialect): string {
  const pk = t.primaryKey.length ? t.primaryKey : t.columns.slice(0, 1).map((c) => c.name)
  const set = t.columns
    .filter((c) => !pk.includes(c.name))
    .map((c) => `  ${q(c.name, dialect)} = ${placeholder(c)}`)
    .join(',\n')
  const where = pk.map((k) => `${q(k, dialect)} = ${placeholder(t.columns.find((c) => c.name === k))}`).join(' AND ')
  return `UPDATE ${tableName(t, dialect)}\nSET\n${set || '  -- no columns'}\nWHERE ${where};`
}

export function generateDeleteTemplate(t: TableInfo, dialect: Dialect): string {
  const pk = t.primaryKey.length ? t.primaryKey : t.columns.slice(0, 1).map((c) => c.name)
  const where = pk.map((k) => `${q(k, dialect)} = ${placeholder(t.columns.find((c) => c.name === k))}`).join(' AND ')
  return `DELETE FROM ${tableName(t, dialect)}\nWHERE ${where};`
}

function placeholder(c?: ColumnInfo): string {
  if (!c) return '?'
  const cat = typeCategory(c.dataType)
  if (cat === 'int' || cat === 'bigint' || cat === 'smallint' || cat === 'decimal' || cat === 'float') return '0'
  if (cat === 'bool') return 'FALSE'
  if (cat === 'date') return "'2024-01-01'"
  if (cat === 'timestamp' || cat === 'timestamptz') return "'2024-01-01 00:00:00'"
  return `'${c.name}'`
}

export interface InsertOptions {
  batchSize?: number
  columnTypes?: (string | undefined)[]
}

/** INSERT statements for concrete rows. batchSize > 1 produces multi-row VALUES lists. */
export function generateInserts(
  target: string,
  columns: string[],
  rows: CellValue[][],
  dialect: Dialect,
  opts: InsertOptions = {}
): string {
  const batch = Math.max(1, opts.batchSize ?? 1)
  const colList = columns.map((c) => q(c, dialect)).join(', ')
  const lit = (row: CellValue[]) => '(' + row.map((v, i) => quoteLiteral(v, dialect, opts.columnTypes?.[i])).join(', ') + ')'
  const out: string[] = []
  if (dialect === 'oracle' && batch > 1) {
    for (let i = 0; i < rows.length; i += batch) {
      const chunk = rows.slice(i, i + batch)
      out.push('INSERT ALL\n' + chunk.map((r) => `  INTO ${target} (${colList}) VALUES ${lit(r)}`).join('\n') + '\nSELECT 1 FROM DUAL;')
    }
    return out.join('\n')
  }
  const max = dialect === 'mssql' ? Math.min(batch, 1000) : batch
  for (let i = 0; i < rows.length; i += max) {
    const chunk = rows.slice(i, i + max)
    if (chunk.length === 1) out.push(`INSERT INTO ${target} (${colList}) VALUES ${lit(chunk[0])};`)
    else out.push(`INSERT INTO ${target} (${colList}) VALUES\n  ${chunk.map(lit).join(',\n  ')};`)
  }
  return out.join('\n')
}

export function generateInsertsForTable(t: TableInfo, rows: Row[], dialect: Dialect, batchSize = 1): string {
  const cols = t.columns.length ? t.columns.map((c) => c.name) : Object.keys(rows[0] ?? {})
  return generateInserts(
    tableName(t, dialect),
    cols,
    rows.map((r) => cols.map((c) => r[c] ?? null)),
    dialect,
    { batchSize, columnTypes: cols.map((c) => typeOf(t, c)) }
  )
}

function keyColumns(t: TableInfo, row: Row): string[] {
  if (t.primaryKey.length) return t.primaryKey
  const uniq = t.indexes.find((i) => i.unique && i.columns.every((c) => c in row))
  if (uniq) return uniq.columns
  return Object.keys(row)
}

function whereFor(t: TableInfo, row: Row, dialect: Dialect): string {
  return keyColumns(t, row)
    .map((k) => (row[k] === null || row[k] === undefined ? `${q(k, dialect)} IS NULL` : `${q(k, dialect)} = ${quoteLiteral(row[k], dialect, typeOf(t, k))}`))
    .join(' AND ')
}

export function generateUpdates(t: TableInfo, rows: Row[], dialect: Dialect): string {
  const pk = t.primaryKey
  return rows
    .map((r) => {
      const set = Object.keys(r)
        .filter((k) => !pk.includes(k))
        .map((k) => `${q(k, dialect)} = ${quoteLiteral(r[k], dialect, typeOf(t, k))}`)
        .join(', ')
      return `UPDATE ${tableName(t, dialect)} SET ${set} WHERE ${whereFor(t, r, dialect)};`
    })
    .join('\n')
}

export function generateDeletes(t: TableInfo, rows: Row[], dialect: Dialect): string {
  return rows.map((r) => `DELETE FROM ${tableName(t, dialect)} WHERE ${whereFor(t, r, dialect)};`).join('\n')
}

/** Upsert (insert or update on key conflict) per dialect. */
export function generateUpserts(t: TableInfo, rows: Row[], dialect: Dialect): string {
  const cols = t.columns.map((c) => c.name)
  const pk = t.primaryKey.length ? t.primaryKey : cols.slice(0, 1)
  const nonPk = cols.filter((c) => !pk.includes(c))
  const target = tableName(t, dialect)
  const colList = cols.map((c) => q(c, dialect)).join(', ')
  return rows
    .map((r) => {
      const vals = cols.map((c) => quoteLiteral(r[c] ?? null, dialect, typeOf(t, c))).join(', ')
      switch (dialect) {
        case 'postgres':
        case 'sqlite':
          return `INSERT INTO ${target} (${colList}) VALUES (${vals})\nON CONFLICT (${pk.map((c) => q(c, dialect)).join(', ')}) ${
            nonPk.length ? `DO UPDATE SET ${nonPk.map((c) => `${q(c, dialect)} = EXCLUDED.${q(c, dialect)}`).join(', ')}` : 'DO NOTHING'
          };`
        case 'mysql':
          return `INSERT INTO ${target} (${colList}) VALUES (${vals})\nON DUPLICATE KEY UPDATE ${(nonPk.length ? nonPk : pk)
            .map((c) => `${q(c, dialect)} = VALUES(${q(c, dialect)})`)
            .join(', ')};`
        default: {
          const src = cols.map((c) => `${quoteLiteral(r[c] ?? null, dialect, typeOf(t, c))} AS ${q(c, dialect)}`).join(', ')
          const on = pk.map((c) => `t.${q(c, dialect)} = s.${q(c, dialect)}`).join(' AND ')
          const upd = nonPk.map((c) => `t.${q(c, dialect)} = s.${q(c, dialect)}`).join(', ')
          const using = dialect === 'oracle' ? `(SELECT ${src} FROM DUAL)` : `(SELECT ${src})`
          return `MERGE INTO ${target} t\nUSING ${using} s\nON (${on})\n${upd ? `WHEN MATCHED THEN UPDATE SET ${upd}\n` : ''}WHEN NOT MATCHED THEN INSERT (${colList}) VALUES (${cols
            .map((c) => `s.${q(c, dialect)}`)
            .join(', ')});`
        }
      }
    })
    .join('\n')
}

export function generateDrop(t: Pick<TableInfo, 'schema' | 'name' | 'kind'>, dialect: Dialect): string {
  const kind = t.kind === 'view' ? 'VIEW' : t.kind === 'materialized-view' ? 'MATERIALIZED VIEW' : 'TABLE'
  return `DROP ${kind} ${tableName(t, dialect)};`
}

// ---------------------------------------------------------------------------------------------
// Types

export type TypeCategory =
  | 'smallint' | 'int' | 'bigint' | 'decimal' | 'float' | 'bool' | 'char' | 'varchar' | 'text'
  | 'date' | 'time' | 'timestamp' | 'timestamptz' | 'binary' | 'json' | 'uuid' | 'other'

export function typeCategory(dataType: string | undefined): TypeCategory {
  const t = (dataType ?? '').toLowerCase().trim()
  if (!t) return 'text'
  if (/^(bool|boolean|bit)$/.test(t) || t === 'tinyint(1)') return 'bool'
  if (/^(smallint|int2|tinyint|smallserial)/.test(t)) return 'smallint'
  if (/^(bigint|int8|bigserial)/.test(t)) return 'bigint'
  if (/^(int|integer|int4|mediumint|serial)/.test(t)) return 'int'
  if (/^(numeric|decimal|number|money|smallmoney|dec)/.test(t)) return 'decimal'
  if (/^(real|float|double|binary_float|binary_double)/.test(t)) return 'float'
  if (/^(uuid|uniqueidentifier)/.test(t)) return 'uuid'
  if (/^(json|jsonb)/.test(t)) return 'json'
  if (/^(bytea|blob|longblob|mediumblob|tinyblob|binary|varbinary|image|raw|long raw)/.test(t)) return 'binary'
  if (/^timestamp.*(with time zone|tz)|^timestamptz|^datetimeoffset|with (local )?time zone/.test(t)) return 'timestamptz'
  if (/^(timestamp|datetime|smalldatetime|datetime2)/.test(t)) return 'timestamp'
  if (/^date$/.test(t)) return 'date'
  if (/^time/.test(t)) return 'time'
  if (/^(varchar|nvarchar|character varying|varchar2|nvarchar2|string)/.test(t)) return /\(max\)/.test(t) ? 'text' : 'varchar'
  if (/^(char|nchar|character|bpchar)\b/.test(t)) return 'char'
  if (/text|clob|citext|xml|ntext|long/.test(t)) return 'text'
  return 'other'
}

function lengthOf(c: ColumnInfo): number | null {
  if (c.maxLength && c.maxLength > 0) return c.maxLength
  const m = /\((\d+)/.exec(c.dataType)
  return m ? Number(m[1]) : null
}

function precisionOf(c: ColumnInfo): [number | null, number | null] {
  if (c.precision) return [c.precision, c.scale ?? 0]
  const m = /\((\d+)\s*(?:,\s*(\d+))?\)/.exec(c.dataType)
  return m ? [Number(m[1]), m[2] ? Number(m[2]) : 0] : [null, null]
}

/** Maps a column type to the target dialect. Same dialect keeps the original type. */
export function mapType(c: ColumnInfo, from: Dialect, to: Dialect): string {
  if (from === to && c.dataType) return c.dataType
  const cat = typeCategory(c.dataType)
  const len = lengthOf(c)
  const [p, s] = precisionOf(c)
  const dec = p ? `(${p}, ${s ?? 0})` : ''
  const T: Record<Dialect, Record<TypeCategory, string>> = {
    postgres: {
      smallint: 'SMALLINT', int: 'INTEGER', bigint: 'BIGINT', decimal: `NUMERIC${dec}`, float: 'DOUBLE PRECISION',
      bool: 'BOOLEAN', char: `CHAR(${len ?? 1})`, varchar: len ? `VARCHAR(${len})` : 'TEXT', text: 'TEXT',
      date: 'DATE', time: 'TIME', timestamp: 'TIMESTAMP', timestamptz: 'TIMESTAMPTZ', binary: 'BYTEA',
      json: 'JSONB', uuid: 'UUID', other: 'TEXT'
    },
    mysql: {
      smallint: 'SMALLINT', int: 'INT', bigint: 'BIGINT', decimal: `DECIMAL${dec || '(38, 10)'}`, float: 'DOUBLE',
      bool: 'TINYINT(1)', char: `CHAR(${len ?? 1})`, varchar: `VARCHAR(${Math.min(len ?? 255, 16383)})`, text: 'LONGTEXT',
      date: 'DATE', time: 'TIME', timestamp: 'DATETIME(6)', timestamptz: 'DATETIME(6)', binary: 'LONGBLOB',
      json: 'JSON', uuid: 'CHAR(36)', other: 'LONGTEXT'
    },
    sqlite: {
      smallint: 'INTEGER', int: 'INTEGER', bigint: 'INTEGER', decimal: 'NUMERIC', float: 'REAL', bool: 'INTEGER',
      char: 'TEXT', varchar: 'TEXT', text: 'TEXT', date: 'TEXT', time: 'TEXT', timestamp: 'TEXT', timestamptz: 'TEXT',
      binary: 'BLOB', json: 'TEXT', uuid: 'TEXT', other: 'TEXT'
    },
    mssql: {
      smallint: 'SMALLINT', int: 'INT', bigint: 'BIGINT', decimal: `DECIMAL${dec || '(38, 10)'}`, float: 'FLOAT',
      bool: 'BIT', char: `NCHAR(${len ?? 1})`, varchar: len && len <= 4000 ? `NVARCHAR(${len})` : 'NVARCHAR(MAX)',
      text: 'NVARCHAR(MAX)', date: 'DATE', time: 'TIME', timestamp: 'DATETIME2', timestamptz: 'DATETIMEOFFSET',
      binary: 'VARBINARY(MAX)', json: 'NVARCHAR(MAX)', uuid: 'UNIQUEIDENTIFIER', other: 'NVARCHAR(MAX)'
    },
    oracle: {
      smallint: 'NUMBER(5)', int: 'NUMBER(10)', bigint: 'NUMBER(19)', decimal: `NUMBER${dec}`, float: 'BINARY_DOUBLE',
      bool: 'NUMBER(1)', char: `CHAR(${len ?? 1})`, varchar: len && len <= 4000 ? `VARCHAR2(${len} CHAR)` : 'CLOB',
      text: 'CLOB', date: 'DATE', time: 'VARCHAR2(20)', timestamp: 'TIMESTAMP', timestamptz: 'TIMESTAMP WITH TIME ZONE',
      binary: 'BLOB', json: 'CLOB', uuid: 'VARCHAR2(36)', other: 'CLOB'
    }
  }
  return T[to][cat]
}

export interface CreateTableOptions {
  targetDialect?: Dialect
  targetSchema?: string
  targetName?: string
  ifNotExists?: boolean
  includeForeignKeys?: boolean
  includeIndexes?: boolean
}

/** Portable CREATE TABLE statement (plus indexes) generated from table metadata. */
export function generateCreateTable(t: TableInfo, from: Dialect, opts: CreateTableOptions = {}): string {
  const to = opts.targetDialect ?? from
  const schema = opts.targetSchema ?? t.schema
  const name = opts.targetName ?? t.name
  const target = qualifiedName(schema, name, to)
  const lines: string[] = []
  const pkSingleAuto = t.primaryKey.length === 1 && t.columns.find((c) => c.name === t.primaryKey[0])?.autoIncrement
  for (const c of t.columns) {
    let type = mapType(c, from, to)
    let extra = ''
    if (c.autoIncrement && from !== to) {
      const cat = typeCategory(c.dataType)
      if (to === 'postgres') type = cat === 'bigint' ? 'BIGINT GENERATED BY DEFAULT AS IDENTITY' : 'INTEGER GENERATED BY DEFAULT AS IDENTITY'
      else if (to === 'mysql') extra = ' AUTO_INCREMENT'
      else if (to === 'mssql') extra = ' IDENTITY(1,1)'
      else if (to === 'oracle') extra = ' GENERATED BY DEFAULT AS IDENTITY'
      else if (to === 'sqlite' && pkSingleAuto) type = 'INTEGER'
    }
    let line = `  ${q(c.name, to)} ${type}${extra}`
    if (!c.nullable) line += ' NOT NULL'
    if (c.defaultValue != null && c.defaultValue !== '' && (from === to || isPortableDefault(c.defaultValue)) && !(c.autoIncrement && from !== to)) {
      line += ` DEFAULT ${c.defaultValue}`
    }
    lines.push(line)
  }
  if (t.primaryKey.length) {
    if (to === 'sqlite' && pkSingleAuto && from !== to) {
      const i = t.columns.findIndex((c) => c.name === t.primaryKey[0])
      lines[i] = lines[i].replace(/ NOT NULL$/, '') + ' PRIMARY KEY AUTOINCREMENT'
    } else {
      lines.push(`  PRIMARY KEY (${t.primaryKey.map((c) => q(c, to)).join(', ')})`)
    }
  }
  if (opts.includeForeignKeys !== false) {
    for (const fk of t.foreignKeys) {
      const refSchema = from === to ? fk.refSchema : schema
      lines.push(
        `  CONSTRAINT ${q(fk.name, to)} FOREIGN KEY (${fk.columns.map((c) => q(c, to)).join(', ')}) REFERENCES ${qualifiedName(refSchema, fk.refTable, to)} (${fk.refColumns
          .map((c) => q(c, to))
          .join(', ')})${fk.onDelete && fk.onDelete !== 'NO ACTION' ? ` ON DELETE ${fk.onDelete}` : ''}${fk.onUpdate && fk.onUpdate !== 'NO ACTION' && to !== 'oracle' ? ` ON UPDATE ${fk.onUpdate}` : ''}`
      )
    }
  }
  const ine = opts.ifNotExists && to !== 'oracle' && to !== 'mssql' ? 'IF NOT EXISTS ' : ''
  let sql = `CREATE TABLE ${ine}${target} (\n${lines.join(',\n')}\n);`
  if (opts.includeIndexes !== false) {
    for (const idx of t.indexes) {
      if (idx.primary) continue
      if (t.primaryKey.length && idx.columns.join(',') === t.primaryKey.join(',')) continue
      sql += `\nCREATE ${idx.unique ? 'UNIQUE ' : ''}INDEX ${q(idx.name, to)} ON ${target} (${idx.columns.map((c) => q(c, to)).join(', ')});`
    }
  }
  if (t.comment && (to === 'postgres' || to === 'oracle')) sql += `\nCOMMENT ON TABLE ${target} IS ${quoteLiteral(t.comment, to)};`
  if (to === 'postgres' || to === 'oracle') {
    for (const c of t.columns) if (c.comment) sql += `\nCOMMENT ON COLUMN ${target}.${q(c.name, to)} IS ${quoteLiteral(c.comment, to)};`
  }
  return sql
}

function isPortableDefault(v: string): boolean {
  return /^(-?\d+(\.\d+)?|'[^']*'|NULL|TRUE|FALSE|CURRENT_TIMESTAMP)$/i.test(v.trim())
}

/** Infers column types from result rows (for "result → CREATE TABLE"). */
export function inferColumns(columns: ColumnMeta[], rows: CellValue[][]): ColumnInfo[] {
  return columns.map((c, i) => {
    if (c.type && typeCategory(c.type) !== 'other') {
      return { name: c.name, dataType: c.type, nullable: true, isPrimaryKey: false }
    }
    let allInt = true
    let allNum = true
    let allBool = true
    let allDate = true
    let allTs = true
    let maxLen = 0
    let any = false
    for (const r of rows) {
      const v = r[i]
      if (v === null || v === undefined || v === '') continue
      any = true
      if (typeof v === 'boolean') {
        allInt = allNum = allDate = allTs = false
        continue
      }
      allBool = false
      const s = String(v)
      maxLen = Math.max(maxLen, s.length)
      if (!/^-?\d+$/.test(s) || s.length > 18) allInt = false
      if (!/^-?\d+(\.\d+)?([eE][-+]?\d+)?$/.test(s)) allNum = false
      if (!/^\d{4}-\d{2}-\d{2}$/.test(s)) allDate = false
      if (!/^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:?\d{2})?$/.test(s)) allTs = false
    }
    let dataType = 'varchar(255)'
    if (!any) dataType = 'text'
    else if (allBool) dataType = 'boolean'
    else if (allInt) dataType = 'bigint'
    else if (allNum) dataType = 'double precision'
    else if (allDate) dataType = 'date'
    else if (allTs) dataType = 'timestamp'
    else if (maxLen > 255) dataType = 'text'
    else dataType = `varchar(${Math.max(16, Math.pow(2, Math.ceil(Math.log2(Math.max(maxLen, 1)))))})`
    return { name: c.name, dataType, nullable: true, isPrimaryKey: false }
  })
}

/** Statements to persist grid edits. Requires a primary key (or unique key) for update/delete. */
export function buildChangeStatements(t: TableInfo, changes: RowChange[], dialect: Dialect): string[] {
  const target = tableName(t, dialect)
  const out: string[] = []
  for (const ch of changes) {
    if (ch.kind === 'delete' && ch.original) {
      out.push(`DELETE FROM ${target} WHERE ${whereFor(t, pick(t, ch.original), dialect)}`)
    } else if (ch.kind === 'update' && ch.original && ch.values) {
      const set = Object.entries(ch.values)
        .map(([k, v]) => `${q(k, dialect)} = ${quoteLiteral(v, dialect, typeOf(t, k))}`)
        .join(', ')
      if (set) out.push(`UPDATE ${target} SET ${set} WHERE ${whereFor(t, pick(t, ch.original), dialect)}`)
    } else if (ch.kind === 'insert' && ch.values) {
      const entries = Object.entries(ch.values).filter(([, v]) => v !== undefined)
      if (!entries.length) {
        out.push(dialect === 'mysql' ? `INSERT INTO ${target} () VALUES ()` : `INSERT INTO ${target} DEFAULT VALUES`)
        continue
      }
      out.push(
        `INSERT INTO ${target} (${entries.map(([k]) => q(k, dialect)).join(', ')}) VALUES (${entries
          .map(([k, v]) => quoteLiteral(v, dialect, typeOf(t, k)))
          .join(', ')})`
      )
    }
  }
  return out
}

/** Reduce the original row to the key columns used in the WHERE clause. */
function pick(t: TableInfo, row: Row): Row {
  const keys = keyColumns(t, row)
  const out: Row = {}
  for (const k of keys) out[k] = row[k] ?? null
  return out
}

export function canEditTable(t: TableInfo): boolean {
  return t.kind === 'table' && (t.primaryKey.length > 0 || t.indexes.some((i) => i.unique))
}
