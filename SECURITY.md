# Sicherheit / Security

SQLighter verwaltet Zugangsdaten zu Datenbanken und verbindet sich mit KI-Diensten. Dieses Dokument beschreibt das Sicherheitsmodell.
*SQLighter handles database credentials and talks to AI services. This document describes its security model (German, with English headings).*

## 1. Transportverschlüsselung / Transport security (MITM protection)

| Modus | Verhalten |
|---|---|
| **Vollständig prüfen** (Standard für entfernte Hosts) | TLS; Zertifikatskette gegen Mozilla- und System-CAs (oder eine eigene CA-Datei, die dann *ausschließlich* gilt) **und** Hostname werden geprüft. |
| **CA prüfen** | TLS; Kette wird geprüft, der Hostname nicht (z. B. Zugriff per IP). |
| **Gepinntes Zertifikat** (PostgreSQL, CockroachDB) | TLS; es wird genau ein Zertifikat (SHA-256 des DER) akzeptiert. Die Handshake-Signatur wird weiterhin geprüft – nur der Besitzer des privaten Schlüssels kann sich verbinden. Das Zertifikat wird per „Zertifikat abrufen“ angezeigt (Subject, Issuer, Gültigkeit, Fingerabdruck) und erst nach Bestätigung vertraut. Ein geändertes Zertifikat bricht die Verbindung mit der Meldung „SERVER CERTIFICATE CHANGED“ ab. |
| **Deaktiviert** | Unverschlüsselt. Für entfernte Hosts nur, wenn ein SSH-Tunnel genutzt wird oder das Risiko pro Verbindung ausdrücklich bestätigt wurde. |

Es gibt bewusst **keinen** Modus „verschlüsseln, aber nicht prüfen“ (`sslmode=require`, `trustServerCertificate=true`), weil er gegen aktive Angreifer keinen Schutz bietet.

TLS wird mit **rustls** umgesetzt (keine OpenSSL-Abhängigkeit). Für native Backup-Werkzeuge werden entsprechende Optionen gesetzt (`PGSSLMODE=verify-full`, `--ssl-mode=VERIFY_IDENTITY`); gepinnte Zertifikate können diese Werkzeuge nicht prüfen, daher wird dort auf den SQL-Dump verwiesen.

Die TLS-Modi sind mit Integrationstests gegen einen echten PostgreSQL-Server mit privater CA abgedeckt (fremde CA → abgelehnt, falscher Hostname → abgelehnt, falscher Pin → abgelehnt).

## 2. SSH-Tunnel

- Implementiert mit **russh**; Authentifizierung per Passwort, privatem Schlüssel (mit Passphrase) oder SSH-Agent.
- **Host-Key-Prüfung**: gespeicherter Fingerabdruck → `~/.ssh/known_hosts` → Bestätigung durch den Benutzer (Trust on First Use).
- **Geänderter Hostschlüssel**: deutliche Warnung („möglicher Man-in-the-Middle-Angriff“); fortfahren nur mit ausdrücklicher Bestätigung und Häkchen „Fingerabdruck mit dem Administrator abgeglichen“.
- PostgreSQL und SQL Server laufen direkt über den SSH-Kanal (kein lokaler Port). MySQL/MariaDB und Oracle nutzen einen Port auf `127.0.0.1`, der nur solange existiert, wie die Verbindung offen ist; das TLS-Zertifikat wird dabei weiterhin gegen den echten Servernamen geprüft.

## 3. Geheimnisse / Secrets

- Passwörter, SSH-Passphrasen, API-Schlüssel und das MCP-Token liegen im **Schlüsselbund des Betriebssystems** (macOS Keychain, Windows Credential Manager, Secret Service/libsecret).
- Konfigurationsdateien (`connections.json`, `settings.json`, `history.json`) enthalten **keine** Geheimnisse, werden atomar geschrieben und sind nur für den eigenen Benutzer lesbar (`0600`, Verzeichnis `0700`).
- Ist kein Schlüsselbund verfügbar, bleiben Geheimnisse nur für die laufende Sitzung im Speicher (Anzeige in der Statusleiste).
- „Passwort nicht speichern“ → Abfrage bei jeder Verbindung.
- Passwörter für native Werkzeuge werden nie über die Kommandozeile übergeben (`PGPASSWORD` bzw. temporäre `--defaults-extra-file` mit `0600`, danach gelöscht).

## 4. Schutz vor Fehlbedienung

- Bestätigung vor `DROP`, `TRUNCATE`, `DELETE`/`UPDATE` ohne `WHERE`, `ALTER … DROP`.
- **Produktions-Verbindungen**: jedes datenverändernde Statement und jede Tabellenänderung erfordert Bestätigung.
- **Read-only-Verbindungen**: in SQLighter blockiert und – wo möglich – zusätzlich auf Datenbankebene (`SET SESSION … READ ONLY`, SQLite read-only geöffnet).
- Tabellenänderungen werden als SQL angezeigt und in **einer Transaktion** ausgeführt (bei Fehler vollständiger Rollback).

## 5. KI-Assistent und Agenten

- **Zugriffsstufen**: *kein Zugriff* (nichts über Datenbanken wird gesendet) · *nur Schema* (Tabellen-/Spaltennamen und Typen) · *Schema + lesende Abfragen*.
- Lesende Abfragen: nur ein einzelnes `SELECT`/`WITH`/`SHOW`/`EXPLAIN` (ohne `ANALYZE`), max. 50 Zeilen, in einer **`READ ONLY`-Transaktion, die immer zurückgerollt wird** (SQLite: `PRAGMA query_only`), auf einer separaten Verbindung, mit Timeout.
- Der eingebaute Assistent erhält **nie** ein Werkzeug zum Ändern von Daten; vorgeschlagenes SQL führt der Benutzer selbst aus (inkl. der Sicherheitsabfragen oben).
- KI-Endpunkte müssen **HTTPS** verwenden; HTTP nur für Loopback-Adressen (z. B. lokales Ollama) oder mit ausdrücklicher Ausnahme pro Anbieter.
- **Claude Code** wird mit `--tools ""` (keine eingebauten Werkzeuge, also keine Shell- oder Dateizugriffe), `--strict-mcp-config` und einer MCP-Konfiguration gestartet, die nur SQLighter enthält. Das dafür verwendete Token ist kurzlebig, an die aktuelle Verbindung und Zugriffsstufe gebunden und wird nach dem Aufruf widerrufen. Der Prompt wird über stdin übergeben.

## 6. MCP-Server

- Lauscht ausschließlich auf `127.0.0.1`.
- Jede Anfrage braucht ein **Bearer-Token** (256 Bit, Vergleich in konstanter Zeit).
- **Host-Header-Prüfung** (Schutz vor DNS-Rebinding) und Ablehnung von Browser-Anfragen mit fremdem `Origin`.
- Anfragegröße begrenzt (1 MB), keine Batch-Requests.
- Schreibzugriff ist standardmäßig **aus**. Wenn aktiviert, muss **jedes** Statement im SQLighter-Fenster freigegeben werden.
- Die Endpunkt-Datei für die Stdio-Bridge ist nur für den eigenen Benutzer lesbar; die Bridge sendet das Token nur an `127.0.0.1`/`localhost`.

## 7. Oberfläche / UI hardening

- Strikte **Content Security Policy** (keine externen Skripte, Styles, Frames oder Verbindungen).
- Die Webview kann nicht von der App weg navigieren; Links aus KI-Antworten werden nur als Text angezeigt; Markdown wird ohne `innerHTML` gerendert.
- Keine Tauri-Plugins für Dateisystem, Shell oder HTTP. Dateien liest/schreibt nur das Backend – und nur Pfade, die der Benutzer in einem **nativen Dateidialog** gewählt hat.

## Sicherheitslücken melden / Reporting vulnerabilities

Bitte melde Sicherheitsprobleme vertraulich über die „Security“-Funktion (Private vulnerability reporting) des GitHub-Repositorys statt über öffentliche Issues.
*Please report security issues privately via the repository's GitHub security advisory feature instead of public issues.*
