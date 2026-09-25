// Virtualized data grid: selection, copy, sort, inline editing, context menu.
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent as RKeyboardEvent } from 'react'
import { ArrowDown, ArrowUp, ClipboardCopy, Eye, FileJson, KeyRound, Table2 } from 'lucide-react'
import type { CellValue, ColumnMeta } from '@shared/types'
import { copyText, displayValue, isNumericType, rawValue, toDelimited, toJson, toMarkdown } from '@/lib/format'
import { t } from '@/lib/i18n'
import { useStore } from '@/lib/store'
import { useMenu, type MenuItem } from './ui'

const ROW_H = 26
const RN_W = 52

export interface GridSelection {
  rows: number[] // data row indices
  cols: number[]
}

export interface GridProps {
  columns: ColumnMeta[]
  rows: CellValue[][]
  editable?: boolean
  pkColumns?: string[]
  edits?: Map<number, Map<number, CellValue>>
  rowState?: (row: number) => 'new' | 'deleted' | undefined
  onEditCell?: (row: number, col: number, value: CellValue) => void
  /** Server-side sort; when omitted the grid sorts client-side. */
  sort?: { col: number; desc: boolean } | null
  onSort?: (col: number) => void
  extraMenu?: (sel: GridSelection) => MenuItem[]
  onSelection?: (sel: GridSelection) => void
  emptyText?: string
}

interface Pos {
  r: number
  c: number
}

function measure(columns: ColumnMeta[], rows: CellValue[][]): number[] {
  const sample = rows.slice(0, 60)
  return columns.map((col, ci) => {
    let len = Math.max(col.name.length + 2, (col.type ?? '').length * 0.8)
    for (const r of sample) len = Math.max(len, Math.min(60, displayValue(r[ci] ?? null, 80).length))
    return Math.round(Math.min(380, Math.max(64, len * 7.4 + 22)))
  })
}

export function ResultGrid(props: GridProps) {
  const { columns, rows, editable } = props
  const scroller = useRef<HTMLDivElement>(null)
  const [scrollTop, setScrollTop] = useState(0)
  const [viewH, setViewH] = useState(400)
  const [widths, setWidths] = useState<number[]>(() => measure(columns, rows))
  const [anchor, setAnchor] = useState<Pos | null>(null)
  const [focus, setFocus] = useState<Pos | null>(null)
  const [editing, setEditing] = useState<{ pos: Pos; value: string; wasNull: boolean } | null>(null)
  const [localSort, setLocalSort] = useState<{ col: number; desc: boolean } | null>(null)
  const menu = useMenu()
  const setDialog = useStore((s) => s.setDialog)
  const toast = useStore((s) => s.toast)

  useEffect(() => {
    setWidths(measure(columns, rows))
    setAnchor(null)
    setFocus(null)
    setLocalSort(null)
    // re-measure only when the column set changes
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [columns])

  useLayoutEffect(() => {
    const el = scroller.current
    if (!el) return
    const ro = new ResizeObserver(() => setViewH(el.clientHeight))
    ro.observe(el)
    setViewH(el.clientHeight)
    return () => ro.disconnect()
  }, [])

  const sort = props.onSort ? props.sort ?? null : localSort
  // display index -> data index
  const order = useMemo(() => {
    const idx = rows.map((_, i) => i)
    if (!props.onSort && localSort) {
      const c = localSort.col
      const numeric = isNumericType(columns[c]?.type)
      idx.sort((a, b) => {
        const va = rows[a][c]
        const vb = rows[b][c]
        if (va === vb) return 0
        if (va === null) return 1
        if (vb === null) return -1
        let r: number
        if (numeric || (typeof va === 'number' && typeof vb === 'number')) r = Number(va) - Number(vb)
        else r = String(va).localeCompare(String(vb), undefined, { numeric: true })
        return localSort.desc ? -r : r
      })
    }
    return idx
  }, [rows, localSort, props.onSort, columns])

  const selRange = useMemo(() => {
    if (!anchor || !focus) return null
    return { r1: Math.min(anchor.r, focus.r), r2: Math.max(anchor.r, focus.r), c1: Math.min(anchor.c, focus.c), c2: Math.max(anchor.c, focus.c) }
  }, [anchor, focus])

  const selection = useCallback((): GridSelection => {
    if (!selRange) return { rows: [], cols: [] }
    const rs: number[] = []
    for (let r = selRange.r1; r <= selRange.r2; r++) if (order[r] !== undefined) rs.push(order[r])
    const cs: number[] = []
    for (let c = selRange.c1; c <= selRange.c2; c++) cs.push(c)
    return { rows: rs, cols: cs }
  }, [selRange, order])

  useEffect(() => {
    props.onSelection?.(selection())
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selRange])

  const valueAt = (dataRow: number, c: number): CellValue => {
    const e = props.edits?.get(dataRow)
    if (e && e.has(c)) return e.get(c) as CellValue
    return rows[dataRow]?.[c] ?? null
  }

  const offsets = useMemo(() => {
    const o: number[] = []
    let x = RN_W
    for (const w of widths) {
      o.push(x)
      x += w
    }
    return { o, total: x }
  }, [widths])

  const ensureVisible = (p: Pos) => {
    const el = scroller.current
    if (!el) return
    const top = p.r * ROW_H
    const headerH = 30
    if (top < el.scrollTop) el.scrollTop = top
    else if (top + ROW_H > el.scrollTop + el.clientHeight - headerH) el.scrollTop = top + ROW_H - el.clientHeight + headerH
    const left = offsets.o[p.c] ?? 0
    const w = widths[p.c] ?? 0
    if (left - RN_W < el.scrollLeft) el.scrollLeft = left - RN_W
    else if (left + w > el.scrollLeft + el.clientWidth) el.scrollLeft = left + w - el.clientWidth
  }

  const selectedText = (withHeaders: boolean, fmt: 'tsv' | 'csv' | 'json' | 'md' = 'tsv') => {
    const sel = selection()
    if (!sel.rows.length) return ''
    const cols = sel.cols.map((c) => columns[c])
    const data = sel.rows.map((r) => sel.cols.map((c) => valueAt(r, c)))
    if (fmt === 'json') return toJson(cols, data)
    if (fmt === 'md') return toMarkdown(cols.map((c) => c.name), data)
    return toDelimited(withHeaders ? cols.map((c) => c.name) : null, data, fmt === 'csv' ? ',' : '\t')
  }

  const copy = async (withHeaders: boolean, fmt: 'tsv' | 'csv' | 'json' | 'md' = 'tsv') => {
    const text = selectedText(withHeaders, fmt)
    if (!text) return
    await copyText(text)
    toast(t('Copied to clipboard'), 'success')
  }

  const startEdit = (p: Pos, initial?: string) => {
    if (!editable || !props.onEditCell) return
    const dataRow = order[p.r]
    if (props.rowState?.(dataRow) === 'deleted') return
    const v = valueAt(dataRow, p.c)
    setEditing({ pos: p, value: initial ?? rawValue(v), wasNull: v === null && initial === undefined })
  }

  const commitEdit = (value: string | null) => {
    if (!editing) return
    const dataRow = order[editing.pos.r]
    const old = valueAt(dataRow, editing.pos.c)
    const next: CellValue = value
    if (!(editing.wasNull && value === '') && next !== old && !(old !== null && String(old) === value)) {
      props.onEditCell?.(dataRow, editing.pos.c, next)
    }
    setEditing(null)
    scroller.current?.focus()
  }

  const viewValue = (p: Pos) => {
    const v = valueAt(order[p.r], p.c)
    setDialog({ type: 'value', title: columns[p.c]?.name ?? '', value: v === null ? 'NULL' : String(v) })
  }

  const onKeyDown = (e: RKeyboardEvent) => {
    if (editing) return
    const mod = e.ctrlKey || e.metaKey
    if (mod && e.key.toLowerCase() === 'c') {
      e.preventDefault()
      copy(e.shiftKey)
      return
    }
    if (mod && e.key.toLowerCase() === 'a') {
      e.preventDefault()
      setAnchor({ r: 0, c: 0 })
      setFocus({ r: rows.length - 1, c: columns.length - 1 })
      return
    }
    if (!focus) return
    const move = (dr: number, dc: number) => {
      e.preventDefault()
      const p = { r: Math.max(0, Math.min(rows.length - 1, focus.r + dr)), c: Math.max(0, Math.min(columns.length - 1, focus.c + dc)) }
      setFocus(p)
      if (!e.shiftKey) setAnchor(p)
      ensureVisible(p)
    }
    const page = Math.max(1, Math.floor(viewH / ROW_H) - 2)
    switch (e.key) {
      case 'ArrowDown':
        return move(1, 0)
      case 'ArrowUp':
        return move(-1, 0)
      case 'ArrowLeft':
        return move(0, -1)
      case 'ArrowRight':
        return move(0, 1)
      case 'PageDown':
        return move(page, 0)
      case 'PageUp':
        return move(-page, 0)
      case 'Home':
        return mod ? move(-rows.length, 0) : move(0, -columns.length)
      case 'End':
        return mod ? move(rows.length, 0) : move(0, columns.length)
      case 'Tab':
        return move(0, e.shiftKey ? -1 : 1)
      case 'Enter':
      case 'F2':
        e.preventDefault()
        if (editable) startEdit(focus)
        else viewValue(focus)
        return
      case 'Delete':
      case 'Backspace':
        if (editable && selRange) {
          e.preventDefault()
          for (let r = selRange.r1; r <= selRange.r2; r++) for (let c = selRange.c1; c <= selRange.c2; c++) props.onEditCell?.(order[r], c, null)
        }
        return
    }
    if (editable && e.key.length === 1 && !mod && !e.altKey) {
      e.preventDefault()
      startEdit(focus, e.key)
    }
  }

  const onCellDown = (e: React.MouseEvent, p: Pos) => {
    if (e.button === 2 && selRange && p.r >= selRange.r1 && p.r <= selRange.r2 && p.c >= selRange.c1 && p.c <= selRange.c2) return
    if (e.shiftKey && anchor) setFocus(p)
    else {
      setAnchor(p)
      setFocus(p)
    }
    if (editing) commitEdit(editing.value)
  }

  const onRowHeader = (e: React.MouseEvent, r: number) => {
    if (e.shiftKey && anchor) {
      setFocus({ r, c: columns.length - 1 })
      setAnchor({ r: anchor.r, c: 0 })
    } else {
      setAnchor({ r, c: 0 })
      setFocus({ r, c: columns.length - 1 })
    }
  }

  const onHeaderClick = (c: number) => {
    if (props.onSort) props.onSort(c)
    else setLocalSort((s) => (s && s.col === c ? (s.desc ? null : { col: c, desc: true }) : { col: c, desc: false }))
  }

  const openMenu = (e: React.MouseEvent) => {
    const sel = selection()
    const items: MenuItem[] = [
      { label: t('Copy'), icon: <ClipboardCopy size={14} />, shortcut: 'Ctrl+C', onClick: () => copy(false), disabled: !sel.rows.length },
      { label: t('Copy with headers'), icon: <Table2 size={14} />, shortcut: 'Ctrl+Shift+C', onClick: () => copy(true), disabled: !sel.rows.length },
      { label: t('Copy as CSV'), onClick: () => copy(true, 'csv'), disabled: !sel.rows.length },
      { label: t('Copy as JSON'), icon: <FileJson size={14} />, onClick: () => copy(true, 'json'), disabled: !sel.rows.length },
      { label: t('Copy as Markdown'), onClick: () => copy(true, 'md'), disabled: !sel.rows.length },
      { separator: true },
      { label: t('View value'), icon: <Eye size={14} />, onClick: () => focus && viewValue(focus), disabled: !focus }
    ]
    const extra = props.extraMenu?.(sel) ?? []
    if (extra.length) items.push({ separator: true }, ...extra)
    menu.open(e, items)
  }

  const first = Math.max(0, Math.floor(scrollTop / ROW_H) - 8)
  const last = Math.min(rows.length, Math.ceil((scrollTop + viewH) / ROW_H) + 8)
  const visible: number[] = []
  for (let i = first; i < last; i++) visible.push(i)
  const numeric = useMemo(() => columns.map((c) => isNumericType(c.type)), [columns])
  const pkSet = useMemo(() => new Set(props.pkColumns ?? []), [props.pkColumns])

  return (
    <div
      className="grid"
      ref={scroller}
      tabIndex={0}
      onScroll={(e) => setScrollTop((e.target as HTMLDivElement).scrollTop)}
      onKeyDown={onKeyDown}
      onContextMenu={openMenu}
    >
      <div className="grid-inner" style={{ width: offsets.total, height: rows.length * ROW_H + 30 }}>
        <div className="grid-header" style={{ width: offsets.total }}>
          <div className="grid-rownum" style={{ width: RN_W }}>
            #
          </div>
          {columns.map((c, ci) => (
            <div
              key={ci}
              className="grid-hcell"
              style={{ width: widths[ci] }}
              title={`${c.name}${c.type ? ` (${c.type})` : ''}`}
              onClick={() => onHeaderClick(ci)}
            >
              <span className="hname">
                {pkSet.has(c.name) && <KeyRound size={11} color="var(--warn)" />}
                <span className="ellipsis">{c.name}</span>
                {sort?.col === ci && (sort.desc ? <ArrowDown size={12} /> : <ArrowUp size={12} />)}
              </span>
              {c.type && <span className="htype">{c.type}</span>}
              <span
                className="resizer"
                onClick={(e) => e.stopPropagation()}
                onMouseDown={(e) => {
                  e.preventDefault()
                  e.stopPropagation()
                  const sx = e.clientX
                  const sw = widths[ci]
                  const mv = (ev: MouseEvent) => setWidths((w) => w.map((x, i) => (i === ci ? Math.max(40, sw + ev.clientX - sx) : x)))
                  const up = () => {
                    window.removeEventListener('mousemove', mv)
                    window.removeEventListener('mouseup', up)
                  }
                  window.addEventListener('mousemove', mv)
                  window.addEventListener('mouseup', up)
                }}
                onDoubleClick={(e) => {
                  e.stopPropagation()
                  setWidths((w) => w.map((x, i) => (i === ci ? measure([c], rows.map((r) => [r[ci]]))[0] : x)))
                }}
              />
            </div>
          ))}
        </div>
        {visible.map((r) => {
          const dataRow = order[r]
          const state = props.rowState?.(dataRow)
          const edits = props.edits?.get(dataRow)
          return (
            <div key={r} className={`grid-row ${state === 'new' ? 'row-new' : ''} ${state === 'deleted' ? 'row-deleted' : ''}`} style={{ top: 30 + r * ROW_H, width: offsets.total }}>
              <div className="grid-rownum" style={{ width: RN_W }} onMouseDown={(e) => onRowHeader(e, r)}>
                {state === 'new' ? '+' : dataRow + 1}
              </div>
              {columns.map((_, ci) => {
                const v = edits?.has(ci) ? (edits.get(ci) as CellValue) : rows[dataRow]?.[ci] ?? null
                const inSel = selRange && r >= selRange.r1 && r <= selRange.r2 && ci >= selRange.c1 && ci <= selRange.c2
                const isFocus = focus && focus.r === r && focus.c === ci
                const isEditing = editing && editing.pos.r === r && editing.pos.c === ci
                const cls = [
                  'grid-cell',
                  v === null ? 'null' : typeof v === 'boolean' ? 'bool' : numeric[ci] || typeof v === 'number' ? 'num' : '',
                  inSel && !(anchor && focus && anchor.r === focus.r && anchor.c === focus.c) ? 'sel' : '',
                  isFocus ? 'focus' : '',
                  edits?.has(ci) ? 'edited' : ''
                ].join(' ')
                return (
                  <div
                    key={ci}
                    className={cls}
                    style={{ width: widths[ci] }}
                    onMouseDown={(e) => onCellDown(e, { r, c: ci })}
                    onMouseEnter={(e) => {
                      if (e.buttons === 1 && anchor) setFocus({ r, c: ci })
                    }}
                    onDoubleClick={() => (editable ? startEdit({ r, c: ci }) : viewValue({ r, c: ci }))}
                  >
                    {isEditing ? (
                      <input
                        autoFocus
                        value={editing.value}
                        placeholder="NULL"
                        onChange={(e) => setEditing({ ...editing, value: e.target.value, wasNull: false })}
                        onBlur={() => commitEdit(editing.value)}
                        onKeyDown={(e) => {
                          e.stopPropagation()
                          if (e.key === 'Enter') {
                            commitEdit(editing.value)
                            const p = { r: Math.min(rows.length - 1, r + 1), c: ci }
                            setAnchor(p)
                            setFocus(p)
                          } else if (e.key === 'Escape') {
                            setEditing(null)
                            scroller.current?.focus()
                          } else if (e.key === 'Tab') {
                            e.preventDefault()
                            commitEdit(editing.value)
                            const p = { r, c: Math.min(columns.length - 1, ci + 1) }
                            setAnchor(p)
                            setFocus(p)
                          }
                        }}
                      />
                    ) : (
                      displayValue(v)
                    )}
                  </div>
                )
              })}
            </div>
          )
        })}
        {!rows.length && (
          <div className="grid-empty" style={{ position: 'absolute', top: 30, left: 0, right: 0 }}>
            {props.emptyText ?? t('No rows')}
          </div>
        )}
      </div>
      {menu.node}
    </div>
  )
}

