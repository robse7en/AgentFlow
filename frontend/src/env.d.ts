/// <reference types="vite/client" />
/** The desktop app's version (from app/src-tauri/tauri.conf.json at build time), or null. */
declare const __APP_VERSION__: string | null
declare module '*.vue' {
  import type { DefineComponent } from 'vue'
  const component: DefineComponent<{}, {}, any>
  export default component
}
