// Shown above an editor / table when the connection to the server was lost.
import { Loader2, RefreshCw, Unplug } from 'lucide-react'
import { cleanError } from '@/lib/errors'
import { t } from '@/lib/i18n'
import { useStore } from '@/lib/store'

export function ConnectionLostBanner({ connectionId }: { connectionId: string | null }) {
  const cs = useStore((s) => (connectionId ? s.conn[connectionId] : undefined))
  if (!connectionId || !cs?.lost) return null
  return (
    <div className="conn-lost" role="alert">
      <Unplug size={15} />
      <div className="grow" style={{ minWidth: 0 }}>
        <div>{t('The connection to the database was lost.')}</div>
        <div className="small ellipsis" title={cs.lost}>
          {cleanError(cs.lost)}
        </div>
      </div>
      <button className="btn small" disabled={cs.reconnecting} onClick={() => useStore.getState().reconnect(connectionId)}>
        {cs.reconnecting ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />} {t('Reconnect')}
      </button>
    </div>
  )
}
