<script setup lang="ts">
import { onMounted, ref, watch } from 'vue'
import { api } from '@/api/client'
import type { DirectoryListing } from '@/api/types'

const props = defineProps<{
  /** When set, browsing is constrained to this directory (project picker). */
  root?: string | null
  start?: string | null
}>()
const emit = defineEmits<{ (e: 'select', path: string, relative: string | null): void; (e: 'cancel'): void }>()

const listing = ref<DirectoryListing | null>(null)
const error = ref<string | null>(null)
const manual = ref('')

async function load(path?: string | null) {
  error.value = null
  try {
    listing.value = await api.listDirectory(path, props.root)
    manual.value = listing.value.path ?? ''
  } catch (e) {
    error.value = (e as Error).message
  }
}

function open(path: string) {
  if (props.root) {
    // Constrained mode: send a repository-relative path so the backend can enforce the boundary.
    const rel = path.startsWith(props.root) ? path.slice(props.root.length).replace(/^[\\/]+/, '') : path
    load(rel)
  } else {
    load(path)
  }
}

function up() {
  if (!listing.value?.parent) return load(props.root ? null : undefined)
  open(listing.value.parent)
}

function choose() {
  if (!listing.value?.path) return
  emit('select', listing.value.path, listing.value.relative ?? null)
}

onMounted(() => load(props.start ?? (props.root ? null : undefined)))
watch(() => props.root, () => load(null))
</script>

<template>
  <div class="picker">
    <div class="crumb">
      <button class="sm" @click="up" :disabled="!listing?.parent && !listing?.path">↑</button>
      <span style="flex: 1; word-break: break-all">{{ listing?.path ?? (root ? root : 'Drives') }}</span>
      <span v-if="listing?.isGitRepo" class="badge COMPLETED">git</span>
    </div>
    <div v-if="error" class="alert error" style="margin: 0.5rem">{{ error }}</div>
    <ul>
      <li v-if="!root && listing?.home && !listing.path" @click="open(listing.home!)"><span>🏠 Home</span><span class="muted small">{{ listing.home }}</span></li>
      <li v-for="entry in listing?.entries ?? []" :key="entry.path" @click="open(entry.path)">
        <span>📁 {{ entry.name }}</span>
      </li>
      <li v-if="listing && !listing.entries.length" class="muted">No sub-directories</li>
    </ul>
    <div class="footer">
      <div v-if="!root" class="input-row" style="flex: 1">
        <input type="text" v-model="manual" placeholder="Or type a path…" @keyup.enter="load(manual)" />
        <button class="sm" @click="load(manual)">Go</button>
      </div>
      <button class="sm" @click="emit('cancel')">Cancel</button>
      <button class="sm primary" @click="choose" :disabled="!listing?.path && !root">
        Select {{ root ? (listing?.relative || 'repository root') : 'this directory' }}
      </button>
    </div>
  </div>
</template>
