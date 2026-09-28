// CodeMirror 6 SQL editor.
import { useEffect, useRef } from 'react'
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap, type Completion, type CompletionContext, type CompletionResult } from '@codemirror/autocomplete'
import { defaultKeymap, history, historyKeymap, indentWithTab, toggleComment } from '@codemirror/commands'
import { bracketMatching, foldGutter, HighlightStyle, indentOnInput, syntaxHighlighting } from '@codemirror/language'
import { MSSQL, MySQL, PLSQL, PostgreSQL, SQLite, sql, type SQLDialect } from '@codemirror/lang-sql'
import { highlightSelectionMatches, searchKeymap } from '@codemirror/search'
import { Compartment, EditorState, Prec } from '@codemirror/state'
import {
  crosshairCursor,
  drawSelection,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  placeholder as placeholderExt,
  rectangularSelection
} from '@codemirror/view'
import { tags } from '@lezer/highlight'
import type { Dialect } from '@shared/types'
import { quoteIdent } from '@shared/sql'
import { clauseAt, statementAt, tableRefs } from '@shared/sqlcontext'

const DIALECTS: Record<Dialect, SQLDialect> = {
  postgres: PostgreSQL,
  mysql: MySQL,
  sqlite: SQLite,
  mssql: MSSQL,
  oracle: PLSQL
}

const theme = EditorView.theme({
  '&': { color: 'var(--text)', backgroundColor: 'var(--bg)', height: '100%' },
  '.cm-content': { fontFamily: 'var(--mono)', caretColor: 'var(--accent)', padding: '8px 0' },
  '.cm-scroller': { fontFamily: 'var(--mono)', lineHeight: '1.6' },
  '.cm-gutters': { backgroundColor: 'var(--bg)', color: 'var(--muted)', border: 'none', paddingLeft: '4px' },
  '.cm-activeLineGutter': { backgroundColor: 'transparent', color: 'var(--text-2)' },
  '.cm-activeLine': { backgroundColor: 'color-mix(in srgb, var(--hover) 45%, transparent)' },
  '&.cm-focused .cm-cursor': { borderLeftColor: 'var(--accent)', borderLeftWidth: '2px' },
  '&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection': { backgroundColor: 'var(--sel) !important' },
  '.cm-selectionMatch': { backgroundColor: 'var(--accent-bg)' },
  '.cm-matchingBracket': { backgroundColor: 'var(--accent-bg)', outline: '1px solid var(--accent)' },
  '.cm-tooltip': { backgroundColor: 'var(--panel)', border: '1px solid var(--border-strong)', borderRadius: '8px', boxShadow: 'var(--shadow)', overflow: 'hidden' },
  '.cm-tooltip-autocomplete > ul > li': { padding: '3px 10px !important', fontFamily: 'var(--mono)', fontSize: '12px' },
  '.cm-tooltip-autocomplete > ul > li[aria-selected]': { backgroundColor: 'var(--accent-bg)', color: 'var(--text)' },
  '.cm-completionDetail': { color: 'var(--muted)', fontStyle: 'normal', marginLeft: '8px' },
  '.cm-panels': { backgroundColor: 'var(--panel)', color: 'var(--text)', borderTop: '1px solid var(--border)' },
  '.cm-panels input, .cm-panels button': { fontSize: '12px' },
  '.cm-textfield': { backgroundColor: 'var(--bg)', border: '1px solid var(--border-strong)', borderRadius: '4px', color: 'var(--text)' },
  '.cm-button': { backgroundImage: 'none', backgroundColor: 'var(--panel-2)', border: '1px solid var(--border-strong)', borderRadius: '4px', color: 'var(--text)' },
  '.cm-searchMatch': { backgroundColor: 'var(--warn-bg)', outline: '1px solid var(--warn)' },
  '.cm-foldGutter span': { color: 'var(--muted)' },
  '.cm-placeholder': { color: 'var(--muted)' },
  '.cm-statement-active': { backgroundColor: 'color-mix(in srgb, var(--accent-bg) 45%, transparent)' }
})

const highlight = HighlightStyle.define([
  { tag: [tags.keyword, tags.operatorKeyword, tags.modifier], color: 'var(--syn-kw)', fontWeight: '500' },
  { tag: [tags.string, tags.special(tags.string)], color: 'var(--syn-str)' },
  { tag: [tags.number, tags.bool, tags.null], color: 'var(--syn-num)' },
  { tag: [tags.comment, tags.lineComment, tags.blockComment], color: 'var(--syn-com)', fontStyle: 'italic' },
  { tag: [tags.function(tags.variableName), tags.standard(tags.name)], color: 'var(--syn-fn)' },
  { tag: [tags.typeName, tags.standard(tags.typeName)], color: 'var(--syn-type)' },
  { tag: [tags.operator, tags.punctuation], color: 'var(--syn-op)' },
  { tag: [tags.special(tags.name), tags.quote], color: 'var(--text)' }
])

/**
 * Completes bare column names of the tables used in the current statement (FROM / JOIN /
 * UPDATE / INSERT INTO) wherever columns belong: SELECT list, WHERE, ON, ORDER BY, SET, …
 * lang-sql itself only completes columns after "table." / "alias.".
 */
function columnCompletion(getProps: () => EditorProps) {
  return (ctx: CompletionContext): CompletionResult | null => {
    const { schema, dialect = 'postgres', defaultSchema } = getProps()
    if (!schema) return null
    const word = ctx.matchBefore(/[\w$À-￿]*/)
    if (!word || (word.from === word.to && !ctx.explicit)) return null
    const prev = ctx.state.sliceDoc(Math.max(0, word.from - 1), word.from)
    if (prev === '.' || prev === '"' || prev === '`' || prev === '[') return null
    // Only the surroundings of the cursor matter; keeps typing fast in long scripts.
    const base = Math.max(0, ctx.pos - 20_000)
    const around = ctx.state.sliceDoc(base, Math.min(ctx.state.doc.length, ctx.pos + 5_000))
    const st = statementAt(around, ctx.pos - base)
    if (clauseAt(st.text, ctx.pos - base - st.offset) !== 'columns') return null
    const keys = Object.keys(schema)
    const lower = new Map(keys.map((k) => [k.toLowerCase(), k]))
    const options: Completion[] = []
    const seen = new Set<string>()
    for (const ref of tableRefs(st.text)) {
      // Tables of the editor's schema are keyed by name; others (schema.table) when present.
      const key =
        (ref.schema && lower.get(`${ref.schema}.${ref.name}`.toLowerCase())) ||
        (!ref.schema || !defaultSchema || ref.schema.toLowerCase() === defaultSchema.toLowerCase() ? lower.get(ref.name.toLowerCase()) : undefined)
      if (!key) continue
      for (const col of schema[key]) {
        const id = `${col}\u0000${ref.alias ?? key}`
        if (seen.has(id)) continue
        seen.add(id)
        const plain = /^[A-Za-z_][\w$]*$/.test(col) && !(dialect === 'postgres' && /[A-Z]/.test(col))
        options.push({ label: col, detail: ref.alias ? `${ref.alias} · ${key}` : key, type: 'property', boost: 20, apply: plain ? col : quoteIdent(col, dialect) })
      }
    }
    return options.length ? { from: word.from, options, validFor: /^[\w$À-￿]*$/ } : null
  }
}

function language(props: EditorProps, getProps: () => EditorProps) {
  const d = DIALECTS[props.dialect ?? 'postgres']
  return [sql({ dialect: d, upperCaseKeywords: true, schema: props.schema, defaultSchema: props.defaultSchema }), d.language.data.of({ autocomplete: columnCompletion(getProps) })]
}

export interface EditorProps {
  value: string
  onChange?: (v: string) => void
  dialect?: Dialect
  schema?: Record<string, string[]>
  defaultSchema?: string
  readOnly?: boolean
  fontSize?: number
  placeholder?: string
  onRun?: (view: EditorView) => void
  onRunScript?: (view: EditorView) => void
  onSave?: () => void
  onFormat?: (view: EditorView) => void
  onExplain?: (view: EditorView) => void
  onReady?: (view: EditorView) => void
}

export function Editor(props: EditorProps) {
  const host = useRef<HTMLDivElement>(null)
  const view = useRef<EditorView | null>(null)
  const langComp = useRef(new Compartment())
  const roComp = useRef(new Compartment())
  const cb = useRef(props)
  cb.current = props

  useEffect(() => {
    if (!host.current) return
    const run = (fn: keyof EditorProps) => (v: EditorView) => {
      const f = cb.current[fn] as ((v: EditorView) => void) | undefined
      if (f) {
        f(v)
        return true
      }
      return false
    }
    const state = EditorState.create({
      doc: props.value,
      extensions: [
        lineNumbers(),
        highlightActiveLineGutter(),
        highlightSpecialChars(),
        history(),
        foldGutter(),
        drawSelection(),
        EditorState.allowMultipleSelections.of(true),
        indentOnInput(),
        syntaxHighlighting(highlight),
        bracketMatching(),
        closeBrackets(),
        autocompletion({ activateOnTyping: true, icons: false }),
        rectangularSelection(),
        crosshairCursor(),
        highlightActiveLine(),
        highlightSelectionMatches(),
        EditorView.lineWrapping,
        placeholderExt(props.placeholder ?? ''),
        theme,
        langComp.current.of(language(props, () => cb.current)),
        roComp.current.of([EditorState.readOnly.of(!!props.readOnly), EditorView.editable.of(!props.readOnly)]),
        Prec.highest(
          keymap.of([
            { key: 'Mod-Enter', run: run('onRun') },
            { key: 'Mod-Shift-Enter', run: run('onRunScript') },
            { key: 'Alt-x', run: run('onRunScript') },
            { key: 'Mod-s', run: () => (cb.current.onSave ? (cb.current.onSave(), true) : false) },
            { key: 'Mod-Shift-f', run: run('onFormat') },
            { key: 'Mod-Shift-e', run: run('onExplain') },
            { key: 'Mod-/', run: toggleComment }
          ])
        ),
        keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...searchKeymap, ...historyKeymap, ...completionKeymap, indentWithTab]),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) cb.current.onChange?.(u.state.doc.toString())
        })
      ]
    })
    const v = new EditorView({ state, parent: host.current })
    view.current = v
    props.onReady?.(v)
    return () => {
      v.destroy()
      view.current = null
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // External value changes (e.g. file opened, AI replaced SQL)
  useEffect(() => {
    const v = view.current
    if (v && props.value !== v.state.doc.toString()) {
      v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: props.value } })
    }
  }, [props.value])

  useEffect(() => {
    view.current?.dispatch({
      effects: langComp.current.reconfigure(language(props, () => cb.current))
    })
  }, [props.dialect, props.schema, props.defaultSchema])

  useEffect(() => {
    view.current?.dispatch({ effects: roComp.current.reconfigure([EditorState.readOnly.of(!!props.readOnly), EditorView.editable.of(!props.readOnly)]) })
  }, [props.readOnly])

  return <div className="editor-wrap" ref={host} style={props.fontSize ? ({ ['--editor-fs' as string]: `${props.fontSize}px` } as React.CSSProperties) : undefined} />
}
