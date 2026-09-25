// Small UI primitives: modal, context menu, splitters, db type icon.
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { X } from 'lucide-react'
import type { DbType } from '@shared/types'

// Only the top-most modal reacts to Escape.
const modalStack: number[] = []
let modalSeq = 0

export function Modal(props: {
  title: ReactNode
  onClose: () => void
  children: ReactNode
  footer?: ReactNode
  size?: 'narrow' | 'wide'
  icon?: ReactNode
  tabs?: ReactNode
}) {
  const [id] = useState(() => ++modalSeq)
  const onClose = useRef(props.onClose)
  onClose.current = props.onClose
  useEffect(() => {
    modalStack.push(id)
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && modalStack[modalStack.length - 1] === id) {
        e.stopPropagation()
        onClose.current()
      }
    }
    window.addEventListener('keydown', onKey, true)
    return () => {
      window.removeEventListener('keydown', onKey, true)
      const i = modalStack.indexOf(id)
      if (i >= 0) modalStack.splice(i, 1)
    }
  }, [id])
  return createPortal(
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div className={`modal ${props.size ?? ''}`} role="dialog" aria-modal="true">
        <div className="modal-head">
          {props.icon}
          <h2>{props.title}</h2>
          <button className="icon-btn" onClick={props.onClose} aria-label="Close">
            <X size={16} />
          </button>
        </div>
        {props.tabs}
        <div className="modal-body">{props.children}</div>
        {props.footer && <div className="modal-foot">{props.footer}</div>}
      </div>
    </div>,
    document.body
  )
}

export interface MenuItem {
  label?: string
  icon?: ReactNode
  shortcut?: string
  danger?: boolean
  disabled?: boolean
  onClick?: () => void
  separator?: boolean
  header?: string
}

export interface MenuState {
  x: number
  y: number
  items: MenuItem[]
}

export function ContextMenu({ menu, onClose }: { menu: MenuState | null; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null)
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null)
  useLayoutEffect(() => {
    if (!menu || !ref.current) return
    const r = ref.current.getBoundingClientRect()
    const x = Math.min(menu.x, window.innerWidth - r.width - 8)
    const y = Math.min(menu.y, window.innerHeight - r.height - 8)
    setPos({ x: Math.max(4, x), y: Math.max(4, y) })
  }, [menu])
  useEffect(() => {
    if (!menu) return
    const close = (e: Event) => {
      if (ref.current && e.target instanceof Node && ref.current.contains(e.target)) return
      onClose()
    }
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('mousedown', close, true)
    window.addEventListener('blur', onClose)
    window.addEventListener('keydown', onKey)
    window.addEventListener('resize', onClose)
    return () => {
      window.removeEventListener('mousedown', close, true)
      window.removeEventListener('blur', onClose)
      window.removeEventListener('keydown', onKey)
      window.removeEventListener('resize', onClose)
    }
  }, [menu, onClose])
  if (!menu) return null
  return createPortal(
    <div className="menu" ref={ref} style={{ left: (pos ?? menu).x, top: (pos ?? menu).y, visibility: pos ? 'visible' : 'hidden' }} onContextMenu={(e) => e.preventDefault()}>
      {menu.items.map((it, i) =>
        it.separator ? (
          <div key={i} className="menu-sep" />
        ) : it.header ? (
          <div key={i} className="menu-label">
            {it.header}
          </div>
        ) : (
          <div
            key={i}
            className={`menu-item ${it.danger ? 'danger' : ''} ${it.disabled ? 'disabled' : ''}`}
            onClick={() => {
              onClose()
              it.onClick?.()
            }}
          >
            {it.icon}
            <span>{it.label}</span>
            {it.shortcut && <span className="shortcut">{it.shortcut}</span>}
          </div>
        )
      )}
    </div>,
    document.body
  )
}

export function useMenu() {
  const [menu, setMenu] = useState<MenuState | null>(null)
  const open = (e: { clientX: number; clientY: number; preventDefault: () => void; stopPropagation?: () => void }, items: MenuItem[]) => {
    e.preventDefault()
    e.stopPropagation?.()
    setMenu({ x: e.clientX, y: e.clientY, items })
  }
  const node = <ContextMenu menu={menu} onClose={() => setMenu(null)} />
  return { open, node, close: () => setMenu(null) }
}

/** Vertical splitter that resizes a neighbouring panel. `sign` = +1 if dragging right grows it. */
export function Splitter({ width, onChange, sign, min = 160, max = 900 }: { width: number; onChange: (w: number) => void; sign: 1 | -1; min?: number; max?: number }) {
  const [drag, setDrag] = useState(false)
  return (
    <div
      className={`splitter ${drag ? 'dragging' : ''}`}
      onMouseDown={(e) => {
        e.preventDefault()
        const startX = e.clientX
        const startW = width
        setDrag(true)
        const move = (ev: MouseEvent) => onChange(Math.max(min, Math.min(max, startW + sign * (ev.clientX - startX))))
        const up = () => {
          setDrag(false)
          window.removeEventListener('mousemove', move)
          window.removeEventListener('mouseup', up)
        }
        window.addEventListener('mousemove', move)
        window.addEventListener('mouseup', up)
      }}
    />
  )
}

/** Horizontal splitter; `onChange` receives the new height of the panel below. */
export function HSplitter({ height, onChange, min = 80, max = 2000 }: { height: number; onChange: (h: number) => void; min?: number; max?: number }) {
  const [drag, setDrag] = useState(false)
  return (
    <div
      className={`hsplitter ${drag ? 'dragging' : ''}`}
      onMouseDown={(e) => {
        e.preventDefault()
        const startY = e.clientY
        const startH = height
        setDrag(true)
        const move = (ev: MouseEvent) => onChange(Math.max(min, Math.min(max, startH - (ev.clientY - startY))))
        const up = () => {
          setDrag(false)
          window.removeEventListener('mousemove', move)
          window.removeEventListener('mouseup', up)
        }
        window.addEventListener('mousemove', move)
        window.addEventListener('mouseup', up)
      }}
    />
  )
}

export function usePersistentState<T>(key: string, initial: T): [T, (v: T) => void] {
  const [v, setV] = useState<T>(() => {
    try {
      const raw = localStorage.getItem(key)
      return raw === null ? initial : (JSON.parse(raw) as T)
    } catch {
      return initial
    }
  })
  return [
    v,
    (nv: T) => {
      setV(nv)
      try {
        localStorage.setItem(key, JSON.stringify(nv))
      } catch {
        /* ignore */
      }
    }
  ]
}

const TYPE_COLORS: Record<DbType, string> = {
  postgres: '#4f8bd6',
  cockroach: '#6933ff',
  mysql: '#e48e00',
  mariadb: '#c0765a',
  sqlite: '#4aa3c8',
  mssql: '#d24b3e',
  oracle: '#e0412f'
}

const TYPE_SHORT: Record<DbType, string> = {
  postgres: 'PG',
  cockroach: 'CR',
  mysql: 'My',
  mariadb: 'Ma',
  sqlite: 'SL',
  mssql: 'MS',
  oracle: 'OR'
}

export function DbIcon({ type, size = 18 }: { type: DbType; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden className="ticon" style={{ flex: 'none' }}>
      <rect x="1" y="1" width="22" height="22" rx="6" fill={TYPE_COLORS[type]} opacity="0.18" />
      <text x="12" y="16" textAnchor="middle" fontSize="10" fontWeight="700" fill={TYPE_COLORS[type]} fontFamily="system-ui, sans-serif">
        {TYPE_SHORT[type]}
      </text>
    </svg>
  )
}

export function Logo({ size = 22 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 1024 1024" className="brand-logo" aria-hidden>
      <defs>
        <linearGradient id="lg" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#7c8cff" />
          <stop offset="1" stopColor="#22c3a6" />
        </linearGradient>
      </defs>
      <g fill="none" stroke="url(#lg)" strokeWidth="72" strokeLinecap="round">
        <ellipse cx="512" cy="300" rx="300" ry="110" />
        <path d="M212 300v230c0 60 134 110 300 110s300-50 300-110V300" />
        <path d="M212 530v200c0 60 134 110 300 110" />
      </g>
      <path d="M760 560 L660 760 L745 760 L700 920 L880 690 L790 690 L845 560 Z" fill="#ffd166" />
    </svg>
  )
}
