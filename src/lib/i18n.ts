// Minimal i18n: English source strings are the keys; German translations below.
// Missing translations fall back to English. Placeholders: {name}.
import { de } from './i18n.de'

export type Lang = 'en' | 'de'

let lang: Lang = detect()

function detect(): Lang {
  const l = (typeof navigator !== 'undefined' ? navigator.language : 'en').toLowerCase()
  return l.startsWith('de') ? 'de' : 'en'
}

export function setLanguage(pref: string) {
  lang = pref === 'de' || pref === 'en' ? pref : detect()
  document.documentElement.lang = lang
}

export function currentLanguage(): Lang {
  return lang
}

export function t(s: string, vars?: Record<string, string | number>): string {
  let out = lang === 'de' ? (de[s] ?? s) : s
  if (vars) for (const [k, v] of Object.entries(vars)) out = out.split(`{${k}}`).join(String(v))
  return out
}
