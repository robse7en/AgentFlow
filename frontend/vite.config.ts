import { readFileSync } from 'node:fs'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// The backend binds 127.0.0.1:8765 by default (see config.json → server).
const backend = process.env.AGENTFLOW_API ?? 'http://127.0.0.1:8765'

// The sidebar shows the desktop app's version: the desktop build bundles this frontend, so the two
// ship together. Null (and nothing shown) when the desktop sources aren't beside it.
const appVersion = (() => {
  try {
    const conf = readFileSync(new URL('../app/src-tauri/tauri.conf.json', import.meta.url), 'utf8')
    return (JSON.parse(conf) as { version?: string }).version ?? null
  } catch {
    return null
  }
})()

export default defineConfig({
  plugins: [vue()],
  define: { __APP_VERSION__: JSON.stringify(appVersion) },
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  server: {
    host: '127.0.0.1',
    port: 5173,
    proxy: {
      '/api': { target: backend, changeOrigin: true, ws: true },
    },
  },
})
