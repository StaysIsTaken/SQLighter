import { describe, expect, it } from 'vitest'
import { cleanError, errorHint } from '../src/lib/errors'

describe('connection errors', () => {
  const refused =
    'Input/output error: Input/output error: Connection refused (os error 61): Input/output error: Connection refused (os error 61)'
  it('removes duplicate prefixes', () => {
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
