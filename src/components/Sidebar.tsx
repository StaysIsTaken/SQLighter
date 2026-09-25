// Left sidebar: connection tree with folders, schemas and objects.
import { useMemo, useState, type DragEvent, type ReactNode } from 'react'
import {
  ArrowRightLeft,
  ChevronDown,
  ChevronRight,
  Columns3,
  Copy,
  Database,
  DatabaseBackup,
  Eye,
  FileCode2,
  Folder as FolderIcon,
  FolderOpen,
  FolderPlus,
  FunctionSquare,
  Hash,
  KeyRound,
  Layers,
  Loader2,
  Pencil,
  Plug,
  PlugZap,
  Plus,
  RefreshCw,
  Search,
  Table2,
  Trash2,
  Unplug,
  Upload,
  Download,
  Wand2,
  Eraser,
  History
} from 'lucide-react'
import type { ColumnInfo, ConnectionView, DbObject, Folder, ObjectKind } from '@shared/types'
import { dialectOf } from '@shared/types'
import { qualifiedName } from '@shared/sql'
import { generateSelect } from '@shared/sqlgen'
import { api, errorMessage } from '@/lib/api'
import { copyText } from '@/lib/format'
import { t } from '@/lib/i18n'
import { useStore } from '@/lib/store'
import { DbIcon, useMenu, usePersistentState, type MenuItem } from './ui'

const DRAG_MIME = 'application/x-sqlighter-node'

type DragNode = { kind: 'folder' | 'connection'; id: string }

const KIND_GROUPS: { kind: ObjectKind; label: string; icon: ReactNode }[] = [
  { kind: 'table', label: 'Tables', icon: <Table2 size={14} /> },
  { kind: 'view', label: 'Views', icon: <Eye size={14} /> },
  { kind: 'materialized-view', label: 'Materialized views', icon: <Layers size={14} /> },
  { kind: 'function', label: 'Functions', icon: <FunctionSquare size={14} /> },
  { kind: 'procedure', label: 'Procedures', icon: <FunctionSquare size={14} /> },
  { kind: 'sequence', label: 'Sequences', icon: <Hash size={14} /> }
]

function byOrder<T extends { name: string; order?: number }>(a: T, b: T) {
  return (a.order ?? 0) - (b.order ?? 0) || a.name.localeCompare(b.name)
}

export function Sidebar() {
  const tree = useStore((s) => s.tree)
  const conn = useStore((s) => s.conn)
  const objects = useStore((s) => s.objects)
  const selected = useStore((s) => s.selectedConnectionId)
  const st = useStore.getState()
  const [expandedArr, setExpandedArr] = usePersistentState<string[]>('sqlighter.tree.expanded', [])
  const expanded = useMemo(() => new Set(expandedArr), [expandedArr])
  const [search, setSearch] = useState('')
  const [dropTarget, setDropTarget] = useState<string | null>(null)
  const [columns, setColumns] = useState<Record<string, ColumnInfo[] | 'loading'>>({})
  const menu = useMenu()

  const toggle = (key: string, force?: boolean) => {
    const n = new Set(expanded)
    const open = force ?? !n.has(key)
    if (open) n.add(key)
    else n.delete(key)
    setExpandedArr([...n])
  }

  const q = search.trim().toLowerCase()

  const matches = (c: ConnectionView) => !q || c.name.toLowerCase().includes(q) || c.host.toLowerCase().includes(q) || c.database.toLowerCase().includes(q)

  const folderHasMatch = (f: Folder): boolean =>
    tree.connections.some((c) => c.folderId === f.id && matches(c)) || tree.folders.some((sub) => sub.parentId === f.id && folderHasMatch(sub))

  // ------------------------------------------------------------------ drag & drop
  const onDragStart = (e: DragEvent, node: DragNode) => {
    e.dataTransfer.setData(DRAG_MIME, JSON.stringify(node))
    e.dataTransfer.effectAllowed = 'move'
  }
  const onDragOver = (e: DragEvent, target: string) => {
    if (!e.dataTransfer.types.includes(DRAG_MIME)) return
    e.preventDefault()
    e.stopPropagation()
    e.dataTransfer.dropEffect = 'move'
    setDropTarget(target)
  }
  const onDrop = async (e: DragEvent, folderId: string | null) => {
    e.preventDefault()
    e.stopPropagation()
    setDropTarget(null)
    const raw = e.dataTransfer.getData(DRAG_MIME)
    if (!raw) return
    const node = JSON.parse(raw) as DragNode
    if (node.kind === 'folder' && node.id === folderId) return
    try {
      await api.moveItem(node.kind, node.id, folderId)
      if (folderId) toggle(`f:${folderId}`, true)
      await st.loadTree()
    } catch (err) {
      st.toast(errorMessage(err), 'error')
    }
  }

  // ------------------------------------------------------------------ actions
  const newFolder = (parentId: string | null) =>
    st.setDialog({
      type: 'prompt',
      title: t('New folder'),
      label: t('Folder name'),
      onSubmit: async (name) => {
        await api.saveFolder({ id: '', name, parentId })
        if (parentId) toggle(`f:${parentId}`, true)
        await st.loadTree()
      }
    })

  const renameFolder = (f: Folder) =>
    st.setDialog({
      type: 'prompt',
      title: t('Rename folder'),
      label: t('Folder name'),
      value: f.name,
      onSubmit: async (name) => {
        await api.saveFolder({ ...f, name })
        await st.loadTree()
      }
    })

  const deleteFolder = (f: Folder) =>
    st.setDialog({
      type: 'confirm',
      title: t('Delete folder "{name}"?', { name: f.name }),
      message: t('Connections inside are kept and moved to the parent folder.'),
      confirmLabel: t('Delete'),
      danger: true,
      onConfirm: async () => {
        await api.deleteFolder(f.id)
        await st.loadTree()
      }
    })

  const deleteConnection = (c: ConnectionView) =>
    st.setDialog({
      type: 'confirm',
      title: t('Delete connection "{name}"?', { name: c.name }),
      message: t('The connection settings and stored credentials are removed. The database itself is not affected.'),
      confirmLabel: t('Delete'),
      danger: true,
      onConfirm: async () => {
        await st.disconnect(c.id)
        await api.deleteConnection(c.id)
        await st.loadTree()
      }
    })

  const openConnection = async (c: ConnectionView) => {
    st.selectConnection(c.id)
    toggle(`c:${c.id}`, true)
    await st.connect(c.id)
  }

  const runDangerous = (c: ConnectionView, title: string, sql: string, after?: () => void) =>
    st.setDialog({
      type: 'confirm',
      title,
      message: t('This cannot be undone. The following statement will be executed on "{name}":', { name: c.name }),
      details: sql,
      confirmLabel: t('Execute'),
      danger: true,
      onConfirm: async () => {
        const r = await api.execute(c.id, sql, { confirmed: true })
        const err = r.results.find((x) => x.error)?.error
        if (err) st.toast(err, 'error')
        else {
          st.toast(t('Done'), 'success')
          after?.()
        }
      }
    })

  const connectionMenu = (c: ConnectionView): MenuItem[] => {
    const cs = conn[c.id]
    const connected = cs?.status === 'connected'
    return [
      connected
        ? { label: t('Disconnect'), icon: <Unplug size={14} />, onClick: () => st.disconnect(c.id) }
        : { label: t('Connect'), icon: <Plug size={14} />, onClick: () => openConnection(c) },
      { label: t('New SQL editor'), icon: <FileCode2 size={14} />, shortcut: 'Ctrl+T', onClick: () => st.openSqlTab({ connectionId: c.id }) },
      { label: t('Refresh'), icon: <RefreshCw size={14} />, disabled: !connected, onClick: () => st.refreshConnection(c.id) },
      { separator: true },
      { label: t('Edit connection…'), icon: <Pencil size={14} />, onClick: () => st.setDialog({ type: 'connection', connection: c }) },
      {
        label: t('Duplicate'),
        icon: <Copy size={14} />,
        onClick: async () => {
          await api.duplicateConnection(c.id)
          await st.loadTree()
        }
      },
      { separator: true },
      { label: t('Import data…'), icon: <Upload size={14} />, onClick: () => st.setDialog({ type: 'import', connectionId: c.id }) },
      { label: t('Transfer data…'), icon: <ArrowRightLeft size={14} />, onClick: () => st.setDialog({ type: 'transfer', sourceConnectionId: c.id }) },
      { label: t('Backup…'), icon: <DatabaseBackup size={14} />, onClick: () => st.setDialog({ type: 'backup', connectionId: c.id }) },
      { label: t('Restore / run SQL file…'), icon: <History size={14} />, onClick: () => st.setDialog({ type: 'restore', connectionId: c.id }) },
      { separator: true },
      { label: t('Delete connection'), icon: <Trash2 size={14} />, danger: true, onClick: () => deleteConnection(c) }
    ]
  }

  const objectMenu = (c: ConnectionView, schema: string, o: DbObject): MenuItem[] => {
    const d = dialectOf(c.type)
    const q = qualifiedName(schema, o.name, d)
    const isRel = o.kind === 'table' || o.kind === 'view' || o.kind === 'materialized-view'
    const items: MenuItem[] = []
    if (isRel) {
      items.push(
        { label: t('Open data'), icon: <Table2 size={14} />, onClick: () => st.openTableTab(c.id, schema, o.name, o.kind, 'data') },
        { label: t('Structure'), icon: <Columns3 size={14} />, onClick: () => st.openTableTab(c.id, schema, o.name, o.kind, 'structure') },
        { label: 'DDL', icon: <FileCode2 size={14} />, onClick: () => st.openTableTab(c.id, schema, o.name, o.kind, 'ddl') },
        {
          label: t('SELECT in new editor'),
          icon: <FileCode2 size={14} />,
          onClick: async () => {
            try {
              const info = await api.describeTable(c.id, schema, o.name, o.kind)
              st.openSqlTab({ connectionId: c.id, schema, sql: generateSelect(info, d) })
            } catch {
              st.openSqlTab({ connectionId: c.id, schema, sql: `SELECT * FROM ${q};` })
            }
          }
        },
        {
          label: t('Generate SQL…'),
          icon: <Wand2 size={14} />,
          onClick: async () => {
            try {
              const info = await api.describeTable(c.id, schema, o.name, o.kind)
              st.setDialog({ type: 'generate', connectionId: c.id, dialect: d, table: info })
            } catch (e) {
              st.toast(errorMessage(e), 'error')
            }
          }
        },
        { separator: true },
        { label: t('Export…'), icon: <Download size={14} />, onClick: () => st.setDialog({ type: 'export', dialect: d, defaultName: o.name, source: { kind: 'table', connectionId: c.id, table: { schema, name: o.name } } }) }
      )
      if (o.kind === 'table') {
        items.push(
          { label: t('Import into table…'), icon: <Upload size={14} />, onClick: () => st.setDialog({ type: 'import', connectionId: c.id, schema, table: o.name }) },
          { label: t('Transfer to other database…'), icon: <ArrowRightLeft size={14} />, onClick: () => st.setDialog({ type: 'transfer', sourceConnectionId: c.id, schema, tables: [o.name] }) }
        )
      }
      items.push({ separator: true })
    } else {
      items.push({ label: t('Show definition'), icon: <FileCode2 size={14} />, onClick: () => st.openTableTab(c.id, schema, o.name, o.kind, 'ddl') }, { separator: true })
    }
    items.push({ label: t('Copy name'), icon: <Copy size={14} />, onClick: () => copyText(q) })
    if (o.kind === 'table') {
      items.push({
        label: t('Truncate…'),
        icon: <Eraser size={14} />,
        danger: true,
        onClick: () => runDangerous(c, t('Empty table {name}?', { name: o.name }), d === 'sqlite' ? `DELETE FROM ${q}` : `TRUNCATE TABLE ${q}`)
      })
    }
    if (isRel) {
      const kw = o.kind === 'view' ? 'VIEW' : o.kind === 'materialized-view' ? 'MATERIALIZED VIEW' : 'TABLE'
      items.push({
        label: t('Drop…'),
        icon: <Trash2 size={14} />,
        danger: true,
        onClick: () => runDangerous(c, t('Drop {name}?', { name: o.name }), `DROP ${kw} ${q}`, () => st.loadObjects(c.id, schema, true))
      })
    }
    return items
  }

  const loadColumns = async (c: ConnectionView, schema: string, o: DbObject) => {
    const key = `${c.id}|${schema}|${o.name}`
    if (columns[key] && columns[key] !== 'loading') return
    setColumns((m) => ({ ...m, [key]: 'loading' }))
    try {
      const info = await api.describeTable(c.id, schema, o.name, o.kind)
      setColumns((m) => ({ ...m, [key]: info.columns }))
    } catch (e) {
      st.toast(errorMessage(e), 'error')
      setColumns((m) => {
        const n = { ...m }
        delete n[key]
        return n
      })
    }
  }

  // ------------------------------------------------------------------ rendering
  const rows: ReactNode[] = []
  const pad = (depth: number) => ({ paddingLeft: 6 + depth * 14 })

  const renderSchema = (c: ConnectionView, schema: string, depth: number) => {
    const key = `${c.id}|${schema}`
    const objs = objects[key]
    if (objs === undefined || objs === 'loading') {
      rows.push(
        <div key={`${key}-loading`} className="tree-row" style={pad(depth)}>
          <span className="twisty" />
          <Loader2 size={13} className="spin muted" />
          <span className="muted small">{t('Loading…')}</span>
        </div>
      )
      // Load lazily after render (no state updates during render).
      if (objs === undefined) queueMicrotask(() => st.loadObjects(c.id, schema))
      return
    }
    if ('error' in objs) {
      rows.push(
        <div key={`${key}-err`} className="tree-row" style={pad(depth)} title={objs.error}>
          <span className="twisty" />
          <span className="small" style={{ color: 'var(--danger)' }}>
            {t('Error loading objects')}
          </span>
        </div>
      )
      return
    }
    for (const g of KIND_GROUPS) {
      const list = objs.filter((o) => o.kind === g.kind && (!q || o.name.toLowerCase().includes(q) || matches(c)))
      if (!list.length) continue
      const gkey = `g:${key}|${g.kind}`
      const open = expanded.has(gkey) || (g.kind === 'table' && !expanded.has(`${gkey}:closed`)) || (!!q && list.length > 0)
      rows.push(
        <div
          key={gkey}
          className="tree-row"
          style={pad(depth)}
          onClick={() => {
            if (g.kind === 'table') toggle(`${gkey}:closed`, open)
            else toggle(gkey)
          }}
        >
          <span className="twisty">{open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
          <span className="ticon">{g.icon}</span>
          <span className="tlabel">{t(g.label)}</span>
          <span className="tmeta">{list.length}</span>
        </div>
      )
      if (!open) continue
      for (const o of list) {
        const okey = `${c.id}|${schema}|${o.name}`
        const isRel = o.kind === 'table' || o.kind === 'view' || o.kind === 'materialized-view'
        const oopen = expanded.has(`o:${okey}`)
        rows.push(
          <div
            key={`o:${okey}`}
            className="tree-row"
            style={pad(depth + 1)}
            title={o.comment ?? o.name}
            onClick={() => st.selectConnection(c.id)}
            onDoubleClick={() => (isRel ? st.openTableTab(c.id, schema, o.name, o.kind, 'data') : st.openTableTab(c.id, schema, o.name, o.kind, 'ddl'))}
            onContextMenu={(e) => menu.open(e, objectMenu(c, schema, o))}
          >
            <span
              className="twisty"
              onClick={(e) => {
                e.stopPropagation()
                if (!isRel) return
                toggle(`o:${okey}`)
                if (!oopen) loadColumns(c, schema, o)
              }}
            >
              {isRel ? oopen ? <ChevronDown size={13} /> : <ChevronRight size={13} /> : null}
            </span>
            <span className="ticon">{KIND_GROUPS.find((x) => x.kind === o.kind)?.icon}</span>
            <span className="tlabel">{o.name}</span>
          </div>
        )
        if (oopen) {
          const cols = columns[okey]
          if (cols === 'loading' || cols === undefined) {
            rows.push(
              <div key={`${okey}-cl`} className="tree-row" style={pad(depth + 2)}>
                <span className="twisty" />
                <Loader2 size={12} className="spin muted" />
              </div>
            )
            if (cols === undefined) queueMicrotask(() => loadColumns(c, schema, o))
          } else {
            for (const col of cols) {
              rows.push(
                <div key={`${okey}|${col.name}`} className="tree-row" style={pad(depth + 2)} title={`${col.name} ${col.dataType}${col.nullable ? '' : ' NOT NULL'}`}>
                  <span className="twisty" />
                  <span className="ticon">{col.isPrimaryKey ? <KeyRound size={12} color="var(--warn)" /> : <Columns3 size={12} />}</span>
                  <span className="tlabel">{col.name}</span>
                  <span className="tmeta mono">{col.dataType}</span>
                </div>
              )
            }
          }
        }
      }
    }
    if (!objs.length) {
      rows.push(
        <div key={`${key}-empty`} className="tree-row" style={pad(depth)}>
          <span className="twisty" />
          <span className="muted small">{t('No objects')}</span>
        </div>
      )
    }
  }

  const renderConnection = (c: ConnectionView, depth: number) => {
    if (!matches(c) && !q) return
    if (q && !matches(c)) return
    const key = `c:${c.id}`
    const cs = conn[c.id]
    const open = expanded.has(key) && cs?.status === 'connected'
    const meta = c.type === 'sqlite' ? (c.filePath ?? '').split(/[\\/]/).pop() : `${c.host}${c.database ? '/' + c.database : ''}`
    rows.push(
      <div
        key={key}
        className={`tree-row ${selected === c.id ? 'selected' : ''}`}
        style={pad(depth)}
        draggable
        onDragStart={(e) => onDragStart(e, { kind: 'connection', id: c.id })}
        onClick={() => {
          st.selectConnection(c.id)
          if (cs?.status === 'connected') toggle(key)
        }}
        onDoubleClick={() => openConnection(c)}
        onContextMenu={(e) => {
          st.selectConnection(c.id)
          menu.open(e, connectionMenu(c))
        }}
        title={`${c.name}\n${meta}`}
      >
        <span
          className="twisty"
          onClick={(e) => {
            e.stopPropagation()
            if (cs?.status === 'connected') toggle(key)
            else openConnection(c)
          }}
        >
          {cs?.status === 'connecting' ? <Loader2 size={13} className="spin" /> : open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        </span>
        {c.color && <span className="conn-color" style={{ background: c.color }} />}
        <DbIcon type={c.type} size={18} />
        <span className="tlabel">{c.name}</span>
        {c.production && <span className="badge danger">PROD</span>}
        {c.readOnly && <span className="badge">RO</span>}
        <span className="tactions">
          <button
            className="icon-btn sm"
            title={t('New SQL editor')}
            onClick={(e) => {
              e.stopPropagation()
              st.openSqlTab({ connectionId: c.id })
            }}
          >
            <FileCode2 size={13} />
          </button>
        </span>
        {cs?.status === 'connected' && <span className="dot" style={{ background: 'var(--ok)' }} title={t('Connected')} />}
        {cs?.status === 'error' && <span className="dot" style={{ background: 'var(--danger)' }} title={cs.error} />}
      </div>
    )
    if (!open || !cs?.summary) return
    const summary = cs.summary
    const schemas = summary.schemas.length ? summary.schemas : [summary.defaultSchema]
    if (schemas.length <= 1) {
      renderSchema(c, schemas[0] ?? summary.defaultSchema, depth + 1)
      return
    }
    // Default schema first, system schemas last.
    const sys = /^(pg_catalog|information_schema|sys|mysql|performance_schema|INFORMATION_SCHEMA|SYS|SYSTEM|crdb_internal|pg_extension)$/
    const sorted = [...schemas].sort((a, b) => Number(b === summary.defaultSchema) - Number(a === summary.defaultSchema) || Number(sys.test(a)) - Number(sys.test(b)) || a.localeCompare(b))
    for (const s of sorted) {
      const skey = `s:${c.id}|${s}`
      const sopen = expanded.has(skey) || (s === summary.defaultSchema && !expanded.has(`${skey}:closed`))
      rows.push(
        <div
          key={skey}
          className="tree-row"
          style={pad(depth + 1)}
          onClick={() => {
            if (s === summary.defaultSchema) toggle(`${skey}:closed`, sopen)
            else toggle(skey)
          }}
          onContextMenu={(e) =>
            menu.open(e, [
              { label: t('New SQL editor'), icon: <FileCode2 size={14} />, onClick: () => st.openSqlTab({ connectionId: c.id, schema: s }) },
              { label: t('Refresh'), icon: <RefreshCw size={14} />, onClick: () => st.loadObjects(c.id, s, true) },
              { separator: true },
              { label: t('Import data…'), icon: <Upload size={14} />, onClick: () => st.setDialog({ type: 'import', connectionId: c.id, schema: s }) },
              { label: t('Transfer data…'), icon: <ArrowRightLeft size={14} />, onClick: () => st.setDialog({ type: 'transfer', sourceConnectionId: c.id, schema: s }) },
              { label: t('Backup schema…'), icon: <DatabaseBackup size={14} />, onClick: () => st.setDialog({ type: 'backup', connectionId: c.id, schema: s }) }
            ])
          }
        >
          <span className="twisty">{sopen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
          <span className="ticon">
            <Database size={14} color={s === summary.defaultSchema ? 'var(--accent)' : undefined} />
          </span>
          <span className="tlabel">{s}</span>
        </div>
      )
      if (sopen) renderSchema(c, s, depth + 2)
    }
  }

  const renderFolder = (f: Folder, depth: number) => {
    if (q && !folderHasMatch(f)) return
    const key = `f:${f.id}`
    const open = expanded.has(key) || !!q
    const count = tree.connections.filter((c) => c.folderId === f.id).length
    rows.push(
      <div
        key={key}
        className={`tree-row ${dropTarget === key ? 'drop-into' : ''}`}
        style={pad(depth)}
        draggable
        onDragStart={(e) => onDragStart(e, { kind: 'folder', id: f.id })}
        onDragOver={(e) => onDragOver(e, key)}
        onDragLeave={() => setDropTarget(null)}
        onDrop={(e) => onDrop(e, f.id)}
        onClick={() => toggle(key)}
        onContextMenu={(e) =>
          menu.open(e, [
            { label: t('New connection here…'), icon: <Plus size={14} />, onClick: () => st.setDialog({ type: 'connection', folderId: f.id }) },
            { label: t('New subfolder…'), icon: <FolderPlus size={14} />, onClick: () => newFolder(f.id) },
            { label: t('Rename…'), icon: <Pencil size={14} />, onClick: () => renameFolder(f) },
            { separator: true },
            { label: t('Delete folder'), icon: <Trash2 size={14} />, danger: true, onClick: () => deleteFolder(f) }
          ])
        }
      >
        <span className="twisty">{open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
        <span className="ticon">{open ? <FolderOpen size={15} /> : <FolderIcon size={15} />}</span>
        <span className="tlabel">{f.name}</span>
        <span className="tmeta">{count || ''}</span>
      </div>
    )
    if (!open) return
    for (const sub of tree.folders.filter((x) => x.parentId === f.id).sort(byOrder)) renderFolder(sub, depth + 1)
    for (const c of tree.connections.filter((x) => x.folderId === f.id).sort(byOrder)) renderConnection(c, depth + 1)
  }

  const folderIds = new Set(tree.folders.map((f) => f.id))
  for (const f of tree.folders.filter((x) => !x.parentId || !folderIds.has(x.parentId)).sort(byOrder)) renderFolder(f, 0)
  for (const c of tree.connections.filter((x) => !x.folderId || !folderIds.has(x.folderId)).sort(byOrder)) renderConnection(c, 0)

  return (
    <>
      <div className="panel-header">
        <span className="panel-title grow">{t('Connections')}</span>
        <button className="icon-btn" title={t('New folder')} onClick={() => newFolder(null)}>
          <FolderPlus size={16} />
        </button>
        <button className="icon-btn" title={t('New connection')} onClick={() => st.setDialog({ type: 'connection' })}>
          <PlugZap size={16} />
        </button>
      </div>
      <div className="search-box">
        <Search size={13} />
        <input className="input" placeholder={t('Search…')} value={search} onChange={(e) => setSearch(e.target.value)} />
      </div>
      <div
        className={`tree ${dropTarget === 'root' ? 'drop-root' : ''}`}
        onDragOver={(e) => onDragOver(e, 'root')}
        onDragLeave={() => setDropTarget(null)}
        onDrop={(e) => onDrop(e, null)}
        onContextMenu={(e) =>
          menu.open(e, [
            { label: t('New connection…'), icon: <Plus size={14} />, onClick: () => st.setDialog({ type: 'connection' }) },
            { label: t('New folder…'), icon: <FolderPlus size={14} />, onClick: () => newFolder(null) }
          ])
        }
      >
        {rows}
        {!tree.connections.length && !tree.folders.length && (
          <div className="tree-empty">
            <Database size={28} style={{ opacity: 0.5 }} />
            <div>{t('No connections yet.')}</div>
            <button className="btn small primary" style={{ marginTop: 10 }} onClick={() => st.setDialog({ type: 'connection' })}>
              <Plus size={13} /> {t('New connection')}
            </button>
          </div>
        )}
      </div>
      {menu.node}
    </>
  )
}

