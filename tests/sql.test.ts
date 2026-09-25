import { describe, expect, it } from 'vitest'
import { analyzeStatement, isReadOnlyStatement, quoteIdent, quoteLiteral, splitStatements, statementAt } from '@shared/sql'

describe('splitStatements', () => {
  it('splits on semicolons outside quotes and comments', () => {
    const s = splitStatements("select 1; select 'a;b'; -- x;\nselect \"c;\"", 'postgres').map((x) => x.text)
    expect(s).toEqual(['select 1', "select 'a;b'", '-- x;\nselect "c;"'])
  })
  it('keeps dollar-quoted function bodies together', () => {
    const s = splitStatements('create function f() returns int as $$ begin return 1; end; $$ language plpgsql; select 2;', 'postgres')
    expect(s).toHaveLength(2)
  })
  it('supports MySQL DELIMITER', () => {
    const s = splitStatements('DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END//\nDELIMITER ;\nSELECT 3;', 'mysql')
    expect(s.map((x) => x.text)).toEqual(['CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END', 'SELECT 3'])
  })
  it('supports SQL Server GO', () => {
    expect(splitStatements('select 1\nGO\nselect 2', 'mssql').map((x) => x.text)).toEqual(['select 1', 'select 2'])
  })
  it('finds the statement at the cursor', () => {
    const sql = 'select 1;\n\nselect 2;\nselect 3'
    expect(statementAt(sql, sql.indexOf('2'), 'postgres')?.text).toBe('select 2')
    expect(statementAt(sql, sql.length, 'postgres')?.text).toBe('select 3')
  })
})

describe('classification', () => {
  it('detects read-only statements conservatively', () => {
    expect(isReadOnlyStatement('SELECT * FROM t')).toBe(true)
    expect(isReadOnlyStatement("select 'drop table x'")).toBe(true)
    expect(isReadOnlyStatement('select * into x from t')).toBe(false)
    expect(isReadOnlyStatement('with d as (delete from t returning *) select * from d')).toBe(false)
    expect(isReadOnlyStatement('select 1; drop table t')).toBe(false)
    expect(isReadOnlyStatement('explain analyze delete from t')).toBe(false)
  })
  it('flags destructive statements', () => {
    expect(analyzeStatement('DELETE FROM t').destructive).toBe(true)
    expect(analyzeStatement('DELETE FROM t WHERE id = 1').destructive).toBe(false)
    expect(analyzeStatement('truncate t').reason).toBe('TRUNCATE')
    expect(analyzeStatement('update t set a = 1').reason).toBe('UPDATE without WHERE')
  })
})

describe('quoting', () => {
  it('quotes identifiers per dialect', () => {
    expect(quoteIdent('a"b', 'postgres')).toBe('"a""b"')
    expect(quoteIdent('a`b', 'mysql')).toBe('`a``b`')
    expect(quoteIdent('a]b', 'mssql')).toBe('[a]]b]')
  })
  it('escapes literals safely', () => {
    expect(quoteLiteral("O'Brien", 'postgres')).toBe("'O''Brien'")
    expect(quoteLiteral('a\\b', 'mysql')).toBe("'a\\\\b'")
    expect(quoteLiteral('Ä', 'mssql')).toBe("N'Ä'")
    expect(quoteLiteral(null, 'oracle')).toBe('NULL')
    expect(quoteLiteral(true, 'postgres')).toBe('TRUE')
    expect(quoteLiteral(true, 'mysql')).toBe('1')
    expect(quoteLiteral('\\xdeadbeef', 'postgres', 'bytea')).toBe("'\\xdeadbeef'::bytea")
  })
})
