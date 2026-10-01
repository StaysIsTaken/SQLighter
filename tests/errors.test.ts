import { describe, expect, it } from 'vitest'
import { cleanError, errorHint, isConnectionLost } from '../src/lib/errors'

describe('connection errors', () => {
  const refused = 'Input/output error: Input/output error: Connection refused (os error 61)'
  it('removes noise prefixes', () => {
    expect(cleanError(refused)).toBe('Connection refused (os error 61)')
  })
  it('explains common failures', () => {
    expect(errorHint(refused)).toMatch(/bind-address/)
    expect(errorHint('Connection timed out (os error 60)')).toMatch(/VPN/)
    expect(errorHint("Access denied for user 'root'@'100.64.0.2' (using password: YES)")).toMatch(/user@host/)
    expect(errorHint("TLS is disabled for the remote host '10.0.0.1'. …")).toMatch(/SSH/)
    expect(errorHint('syntax error at or near "x"')).toBeNull()
  })
})

describe('lost connections', () => {
  it('recognises dropped connections', () => {
    expect(isConnectionLost("Input/output error: Input/output error: Driver error: `Connection to the server is closed.'")).toBe(true)
    expect(isConnectionLost('MySQL server has gone away')).toBe(true)
    expect(isConnectionLost('Connection reset by peer (os error 54)')).toBe(true)
    expect(isConnectionLost('Broken pipe (os error 32)')).toBe(true)
    expect(isConnectionLost('Connection to the server was lost and could not be re-established: connection timed out')).toBe(true)
  })
  it('ignores statement errors', () => {
    expect(isConnectionLost("Table 'jobrouter.accounting' doesn't exist")).toBe(false)
    expect(isConnectionLost('syntax error at or near "x"')).toBe(false)
  })
})
