#!/usr/bin/env node
// SQLighter MCP bridge: connects stdio-based MCP clients (Claude Desktop / Cowork, ...) to the
// MCP server running inside SQLighter. No dependencies; requires Node.js 18+.
//
// Configuration (Claude Desktop → Settings → Developer → Edit config):
//   { "mcpServers": { "sqlighter": { "command": "node", "args": ["<path>/sqlighter-mcp-bridge.mjs"],
//       "env": { "SQLIGHTER_MCP_ENDPOINT": "<SQLighter data dir>/mcp-endpoint.json" } } } }
//
// The endpoint file (URL + token) is written by SQLighter when "MCP server" is enabled in the
// settings and is readable by the current user only. SQLighter must be running.

import { readFileSync } from 'node:fs'
import { homedir, platform } from 'node:os'
import { join } from 'node:path'
import { createInterface } from 'node:readline'

const APP_ID = 'dev.sqlighter.app'

function defaultEndpointFile() {
  const home = homedir()
  switch (platform()) {
    case 'darwin':
      return join(home, 'Library', 'Application Support', APP_ID, 'mcp-endpoint.json')
    case 'win32':
      return join(process.env.APPDATA || join(home, 'AppData', 'Roaming'), APP_ID, 'mcp-endpoint.json')
    default:
      return join(process.env.XDG_DATA_HOME || join(home, '.local', 'share'), APP_ID, 'mcp-endpoint.json')
  }
}

function endpoint() {
  const file = process.env.SQLIGHTER_MCP_ENDPOINT || defaultEndpointFile()
  try {
    const { url, token } = JSON.parse(readFileSync(file, 'utf8'))
    const u = new URL(url)
    // Never send the token anywhere but the local machine.
    if (u.hostname !== '127.0.0.1' && u.hostname !== 'localhost') throw new Error('endpoint must be on 127.0.0.1')
    return { url, token }
  } catch (e) {
    return { error: `SQLighter MCP endpoint not available (${file}): ${e.message}. Start SQLighter and enable the MCP server in Settings → AI.` }
  }
}

function send(msg) {
  process.stdout.write(JSON.stringify(msg) + '\n')
}

const rl = createInterface({ input: process.stdin, crlfDelay: Infinity })
rl.on('line', async (line) => {
  if (!line.trim()) return
  let msg
  try {
    msg = JSON.parse(line)
  } catch {
    send({ jsonrpc: '2.0', id: null, error: { code: -32700, message: 'parse error' } })
    return
  }
  const ep = endpoint()
  if (ep.error) {
    if (msg.id !== undefined) send({ jsonrpc: '2.0', id: msg.id, error: { code: -32000, message: ep.error } })
    return
  }
  try {
    const res = await fetch(ep.url, {
      method: 'POST',
      headers: { 'content-type': 'application/json', accept: 'application/json, text/event-stream', authorization: `Bearer ${ep.token}` },
      body: line
    })
    if (res.status === 202) return
    const text = await res.text()
    if (!res.ok) {
      if (msg.id !== undefined) send({ jsonrpc: '2.0', id: msg.id, error: { code: -32000, message: `SQLighter: HTTP ${res.status} ${text}` } })
      return
    }
    process.stdout.write(text.trim() + '\n')
  } catch (e) {
    if (msg.id !== undefined) send({ jsonrpc: '2.0', id: msg.id, error: { code: -32000, message: `SQLighter is not reachable: ${e.message}` } })
  }
})
