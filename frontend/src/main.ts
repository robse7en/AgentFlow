import { createApp } from 'vue'
import { createPinia } from 'pinia'
import App from './App.vue'
import { router } from './router'
// Bundled rather than fetched from Google Fonts: the desktop app runs offline.
import '@fontsource-variable/inter/wght.css'
import '@fontsource/jetbrains-mono/latin-400.css'
import '@fontsource/jetbrains-mono/latin-500.css'
import './styles/tokens.css'
import './styles.css'
import '@vue-flow/core/dist/style.css'
import '@vue-flow/core/dist/theme-default.css'
import '@vue-flow/minimap/dist/style.css'

createApp(App).use(createPinia()).use(router).mount('#app')
