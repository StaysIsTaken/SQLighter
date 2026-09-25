import { StrictMode, useEffect, useState } from 'react'
import { createRoot } from 'react-dom/client'
import { App } from './App'
import { applyTheme } from './components/dialogs/SettingsDialog'
import { errorMessage } from './lib/api'
import { setLanguage } from './lib/i18n'
import { useStore } from './lib/store'
import './styles.css'

function Root() {
  const [ready, setReady] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const language = useStore((s) => s.settings?.language)
  useEffect(() => {
    const st = useStore.getState()
    Promise.all([st.loadSettings(), st.loadTree()])
      .then(() => {
        const s = useStore.getState().settings
        if (s) {
          setLanguage(s.language)
          applyTheme(s.theme)
          window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => applyTheme(useStore.getState().settings?.theme ?? 'system'))
        }
        setReady(true)
      })
      .catch((e) => setError(errorMessage(e)))
  }, [])
  if (error) return <div style={{ padding: 40, fontFamily: 'system-ui' }}>SQLighter could not start: {error}</div>
  if (!ready) return null
  // Re-mount the UI when the language changes so every label is re-rendered.
  return <App key={language} />
}

// Last-resort error display (e.g. if the backend is unreachable during startup).
function showFatal(msg: string) {
  const root = document.getElementById('root')
  if (root && !root.childElementCount) {
    root.innerText = `SQLighter failed to start:\n${msg}`
    root.style.cssText = 'padding:40px;font-family:system-ui;white-space:pre-wrap;color:#e6e8ee'
  }
}
window.addEventListener('error', (e) => showFatal(String(e.error?.stack ?? e.message)))
// eslint-disable-next-line no-console
console.info('SQLighter UI starting')
window.addEventListener('unhandledrejection', (e) => showFatal(String((e.reason as Error)?.stack ?? e.reason)))

// Block the browser's default context menu except in text inputs.
document.addEventListener('contextmenu', (e) => {
  const el = e.target as HTMLElement
  if (!el.closest('input, textarea, .cm-content')) e.preventDefault()
})

applyTheme('system')
createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <Root />
  </StrictMode>
)
