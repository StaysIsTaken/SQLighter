// Turns raw driver/network errors into a readable message plus a hint on what to check.
import { t } from './i18n'

/** Removes repeated prefixes like "Input/output error: Input/output error: …" and duplicate segments. */
export function cleanError(msg: string): string {
  const parts = msg
    .split(/:\s+/)
    .map((p) => p.trim())
    .filter(Boolean)
  const out: string[] = []
  for (const p of parts) {
    if (/^(input\/output error|io error)$/i.test(p)) continue
    if (out.some((o) => o.toLowerCase() === p.toLowerCase())) continue
    out.push(p)
  }
  return out.join(': ') || msg
}

export function errorHint(msg: string): string | null {
  const m = msg.toLowerCase()
  if (/tls is disabled for the remote host/.test(m))
    return t('Enable TLS, use an SSH tunnel, or acknowledge the risk in the "TLS / Security" tab.')
  if (/connection refused|os error (61|111|10061)\b|actively refused/.test(m))
    return t(
      'The host is reachable, but nothing accepts connections on this port. Check that the database server is running, listens on the network (MySQL/MariaDB: bind-address, PostgreSQL: listen_addresses) and that no firewall blocks the port. Alternatively connect via SSH tunnel with database host 127.0.0.1.'
    )
  if (/timed out|timeout|os error (60|110|10060)\b/.test(m))
    return t('No answer from the host. Check the address, the VPN connection and firewalls between you and the server.')
  if (/no route to host|network is unreachable|host is unreachable|os error (51|65|101|113|10051|10065)\b/.test(m))
    return t('The host cannot be reached. Check the address and whether the VPN is connected.')
  if (/failed to lookup address|name or service not known|nodename nor servname|no such host|could not resolve|dns/.test(m))
    return t('The host name could not be resolved. Check the spelling or use the IP address.')
  if (/access denied|password authentication failed|login failed|invalid username\/password|ora-01017|authentication failed/.test(m))
    return t(
      'The server rejected the login. Check user and password, and whether the user may connect from your address (MySQL/MariaDB: user@host, PostgreSQL: pg_hba.conf).'
    )
  if (/certificate|unknownissuer|notvalidforname|invalid peer certificate/.test(m))
    return t('The server certificate could not be verified. Check the CA file, the host name, or use "Pinned certificate" in the TLS tab.')
  return null
}

/** Clean message with an optional hint, for toasts. */
export function describeError(msg: string): string {
  const hint = errorHint(msg)
  const clean = cleanError(msg)
  return hint ? `${clean}\n${hint}` : clean
}
