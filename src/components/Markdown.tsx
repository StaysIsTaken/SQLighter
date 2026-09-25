// Safe markdown renderer (no HTML injection): paragraphs, headings, lists, tables, inline code,
// bold/italic and fenced code blocks with custom actions.
import type { ReactNode } from 'react'

export interface CodeBlockRenderer {
  (code: string, lang: string, key: number): ReactNode
}

function inline(text: string, keyBase: string): ReactNode[] {
  const out: ReactNode[] = []
  // `code`, **bold**, *italic*, [text](url)
  const re = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*\s][^*]*\*)|(\[[^\]]+\]\([^)]+\))/g
  let last = 0
  let m: RegExpExecArray | null
  let i = 0
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index))
    const tok = m[0]
    const k = `${keyBase}-${i++}`
    if (tok.startsWith('`')) out.push(<code key={k}>{tok.slice(1, -1)}</code>)
    else if (tok.startsWith('**')) out.push(<strong key={k}>{tok.slice(2, -2)}</strong>)
    else if (tok.startsWith('*')) out.push(<em key={k}>{tok.slice(1, -1)}</em>)
    else {
      // Links are shown as text + URL; the app never navigates to external pages.
      const lm = /^\[([^\]]+)\]\(([^)]+)\)$/.exec(tok)
      out.push(
        <span key={k} title={lm?.[2]}>
          {lm?.[1]} <span className="muted small">({lm?.[2]})</span>
        </span>
      )
    }
    last = m.index + tok.length
  }
  if (last < text.length) out.push(text.slice(last))
  return out
}

export function Markdown({ text, code }: { text: string; code: CodeBlockRenderer }) {
  const blocks: ReactNode[] = []
  const lines = text.split('\n')
  let i = 0
  let key = 0
  while (i < lines.length) {
    const line = lines[i]
    const fence = /^\s*```\s*([\w+-]*)\s*$/.exec(line)
    if (fence) {
      const lang = fence[1] || ''
      const body: string[] = []
      i++
      while (i < lines.length && !/^\s*```\s*$/.test(lines[i])) body.push(lines[i++])
      i++ // closing fence (or end while streaming)
      blocks.push(code(body.join('\n'), lang.toLowerCase(), key++))
      continue
    }
    const h = /^(#{1,4})\s+(.*)$/.exec(line)
    if (h) {
      const k = key++
      const content = inline(h[2], `h${k}`)
      blocks.push(h[1].length <= 1 ? <h2 key={k}>{content}</h2> : <h3 key={k}>{content}</h3>)
      i++
      continue
    }
    if (/^\s*[-*]\s+/.test(line) || /^\s*\d+[.)]\s+/.test(line)) {
      const ordered = /^\s*\d+[.)]\s+/.test(line)
      const items: string[] = []
      while (i < lines.length && (/^\s*[-*]\s+/.test(lines[i]) || /^\s*\d+[.)]\s+/.test(lines[i]))) {
        items.push(lines[i].replace(/^\s*([-*]|\d+[.)])\s+/, ''))
        i++
      }
      const k = key++
      const lis = items.map((it, j) => <li key={j}>{inline(it, `l${k}-${j}`)}</li>)
      blocks.push(ordered ? <ol key={k}>{lis}</ol> : <ul key={k}>{lis}</ul>)
      continue
    }
    if (/^\s*\|.*\|\s*$/.test(line) && i + 1 < lines.length && /^\s*\|[\s:|-]+\|\s*$/.test(lines[i + 1])) {
      const split = (l: string) => l.trim().replace(/^\||\|$/g, '').split('|').map((c) => c.trim())
      const head = split(line)
      i += 2
      const body: string[][] = []
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) body.push(split(lines[i++]))
      const k = key++
      blocks.push(
        <table key={k}>
          <thead>
            <tr>
              {head.map((c, j) => (
                <th key={j}>{inline(c, `t${k}h${j}`)}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {body.map((r, ri) => (
              <tr key={ri}>
                {r.map((c, j) => (
                  <td key={j}>{inline(c, `t${k}r${ri}c${j}`)}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      )
      continue
    }
    if (!line.trim()) {
      i++
      continue
    }
    const para: string[] = []
    while (i < lines.length && lines[i].trim() && !/^\s*```/.test(lines[i]) && !/^#{1,4}\s/.test(lines[i]) && !/^\s*([-*]|\d+[.)])\s+/.test(lines[i])) para.push(lines[i++])
    const k = key++
    blocks.push(
      <p key={k}>
        {para.map((p, j) => (
          <span key={j}>
            {inline(p, `p${k}-${j}`)}
            {j < para.length - 1 && <br />}
          </span>
        ))}
      </p>
    )
  }
  return <>{blocks}</>
}
