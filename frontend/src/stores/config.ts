import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { api } from '@/api/client'
import type { HarnessInfo, SystemConfig } from '@/api/types'

export const useConfigStore = defineStore('config', () => {
  const config = ref<SystemConfig | null>(null)
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function load() {
    loading.value = true
    try {
      config.value = await api.config()
      error.value = null
    } catch (e) {
      error.value = (e as Error).message
    } finally {
      loading.value = false
    }
  }

  async function reload() {
    loading.value = true
    try {
      config.value = await api.reloadConfig()
      error.value = null
    } catch (e) {
      error.value = (e as Error).message
      throw e
    } finally {
      loading.value = false
    }
  }

  async function detectHarnesses() {
    const harnesses = await api.detectHarnesses()
    if (config.value) config.value = { ...config.value, harnesses }
  }

  async function setDefaultRepository(path: string | null) {
    config.value = await api.setDefaultRepository(path)
  }

  async function setExchangeRate(usdToNzdRate: number) {
    config.value = await api.setExchangeRate(usdToNzdRate)
  }

  const setCostControls = async (controls: unknown, billing: Record<string, string>) => {
    config.value = await api.setCostControls(controls, billing);
  };

  const harnesses = computed<HarnessInfo[]>(() => config.value?.harnesses ?? [])
  const usableHarnesses = computed(() => harnesses.value.filter((h) => h.enabled && h.available))
  const harnessByKey = computed(() => (key: string) => harnesses.value.find((h) => h.key === key))

  return {
    config, loading, error, harnesses, usableHarnesses, harnessByKey,
    load, reload, detectHarnesses, setDefaultRepository, setExchangeRate, setCostControls,
  }
})
