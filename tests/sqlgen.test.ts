import { describe, expect, it } from 'vitest'
import type { TableInfo } from '@shared/types'
import { buildChangeStatements, canEditTable, generateCreateTable, generateInserts, generateSelect, generateUpserts, inferColumns, mapType } from '@shared/sqlgen'

const users: TableInfo = {
  schema: 'public',
  name: 'users',
  kind: 'table',
  columns: [
    { name: 'id', dataType: 'integer', nullable: false, isPrimaryKey: true, autoIncrement: true },
    { name: 'name', dataType: 'character varying(100)', nullable: false, isPrimaryKey: false },
    { name: 'active', dataType: 'boolean', nullable: true, isPrimaryKey: false, defaultValue: 'true' }
  ],
  primaryKey: ['id'],
  indexes: [{ name: 'users_name_idx', columns: ['name'], unique: false }],
  foreignKeys: []
}

describe('sql generator', () => {
  it('generates SELECT with dialect specific limits', () => {
    expect(generateSelect(users, 'postgres')).toContain('LIMIT 100')
    expect(generateSelect(users, 'mssql')).toContain('SELECT TOP 100')
    expect(generateSelect(users, 'oracle')).toContain('FETCH FIRST 100 ROWS ONLY')
  })
  it('generates batched INSERTs', () => {
    const sql = generateInserts('"t"', ['a', 'b'], [[1, "x'y"], [2, null]], 'postgres', { batchSize: 10 })
    expect(sql).toBe(`INSERT INTO "t" ("a", "b") VALUES\n  (1, 'x''y'),\n  (2, NULL);`)
  })
  it('converts CREATE TABLE between dialects', () => {
    const my = generateCreateTable(users, 'postgres', { targetDialect: 'mysql', targetSchema: '' })
    expect(my).toContain('`id` INT AUTO_INCREMENT NOT NULL')
    expect(my).toContain('`name` VARCHAR(100) NOT NULL')
    expect(my).toContain('`active` TINYINT(1) DEFAULT true')
    expect(my).toContain('CREATE INDEX `users_name_idx`')
    expect(mapType({ name: 'x', dataType: 'jsonb', nullable: true, isPrimaryKey: false }, 'postgres', 'mssql')).toBe('NVARCHAR(MAX)')
  })
  it('builds upserts per dialect', () => {
    const rows = [{ id: 1, name: 'a', active: true }]
    expect(generateUpserts(users, rows, 'postgres')).toContain('ON CONFLICT ("id") DO UPDATE SET')
    expect(generateUpserts(users, rows, 'mysql')).toContain('ON DUPLICATE KEY UPDATE')
    expect(generateUpserts(users, rows, 'mssql')).toContain('MERGE INTO')
  })
  it('turns grid edits into keyed statements', () => {
    const st = buildChangeStatements(
      users,
      [
        { kind: 'update', original: { id: 7, name: 'old', active: true }, values: { name: 'new' } },
        { kind: 'delete', original: { id: 8, name: 'x', active: null } },
        { kind: 'insert', values: { name: 'z' } }
      ],
      'postgres'
    )
    expect(st).toEqual([
      `UPDATE "public"."users" SET "name" = 'new' WHERE "id" = 7`,
      `DELETE FROM "public"."users" WHERE "id" = 8`,
      `INSERT INTO "public"."users" ("name") VALUES ('z')`
    ])
    expect(canEditTable(users)).toBe(true)
    expect(canEditTable({ ...users, primaryKey: [], indexes: [] })).toBe(false)
  })
  it('infers column types from result rows', () => {
    const cols = inferColumns([{ name: 'n' }, { name: 'd' }, { name: 's' }], [[1, '2024-01-01', 'abc'], [22, '2024-02-02', null]])
    expect(cols.map((c) => c.dataType)).toEqual(['bigint', 'date', 'varchar(16)'])
  })
})
