import { describe, expect, it } from 'vitest'
import { clauseAt, statementAt, tableRefs } from '../src/shared/sqlcontext'

const at = (sql: string) => clauseAt(sql.replace('|', ''), sql.indexOf('|'))

describe('sql context', () => {
  it('finds tables and aliases', () => {
    expect(tableRefs('SELECT * FROM jrusers u LEFT JOIN jruserjob AS uj ON uj.username = u.username WHERE ')).toEqual([
      { name: 'jrusers', alias: 'u' },
      { name: 'jruserjob', alias: 'uj' }
    ])
    expect(tableRefs('SELECT * FROM jobrouter.jrusers WHERE')).toEqual([{ schema: 'jobrouter', name: 'jrusers' }])
    expect(tableRefs('select * from a x, "Other Table" y where')).toEqual([
      { name: 'a', alias: 'x' },
      { name: 'Other Table', alias: 'y' }
    ])
    expect(tableRefs('UPDATE [dbo].[Users] SET ')).toEqual([{ schema: 'dbo', name: 'Users' }])
    expect(tableRefs('INSERT INTO orders (id, ')).toEqual([{ name: 'orders' }])
    expect(tableRefs('SELECT * FROM users WHERE id IN (SELECT user_id FROM orders o)')).toEqual([{ name: 'users' }, { name: 'orders', alias: 'o' }])
    expect(tableRefs('SELECT * FROM users WHERE id IN (SELECT user_id FROM orders o)', true)).toEqual([{ name: 'users' }])
  })

  it('knows where columns belong', () => {
    expect(at('SELECT * FROM jrusers WHERE |')).toBe('columns')
    expect(at('SELECT * FROM jrusers WHERE us|')).toBe('columns')
    expect(at('SELECT * FROM t WHERE a = 1 AND |')).toBe('columns')
    expect(at('SELECT | FROM t')).toBe('columns')
    expect(at('SELECT a, b| FROM t')).toBe('columns')
    expect(at('SELECT * FROM a JOIN b ON |')).toBe('columns')
    expect(at('SELECT * FROM t ORDER BY |')).toBe('columns')
    expect(at('UPDATE t SET |')).toBe('columns')
    expect(at('INSERT INTO t (|')).toBe('columns')
    expect(at('SELECT * FROM |')).toBe('tables')
    expect(at('SELECT * FROM t JOIN |')).toBe('tables')
    expect(at('SELECT * FROM t u WHERE u.|')).toBe('member')
    expect(at("SELECT * FROM t WHERE name = 'ab|")).toBe('other')
    expect(at('SELECT * FROM t WHERE x IN (SELECT |')).toBe('columns')
  })

  it('splits statements outside strings', () => {
    const doc = "SELECT ';' FROM a;\nSELECT * FROM b WHERE "
    const s = statementAt(doc, doc.length)
    expect(s.text.trim()).toBe('SELECT * FROM b WHERE')
    expect(statementAt(doc, 3).text).toBe("SELECT ';' FROM a")
  })
})
