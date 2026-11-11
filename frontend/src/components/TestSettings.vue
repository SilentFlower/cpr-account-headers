<script setup lang="ts">
import type { SharedTestSettings, TestGroup } from '../api/modules/testing'
import { ZButton, ZDialog, ZFormItem } from '@codex-proxy/ui'
import { ZSelect } from '@codex-proxy/ui/select'
import { computed, onBeforeUnmount, shallowRef, watch } from 'vue'
import { getTestSettings, saveTestSettings } from '../api/modules/testing'

const emit = defineEmits<{ saved: [] }>()
const open = defineModel<boolean>({ required: true })
const settings = shallowRef<SharedTestSettings>()
const groups = shallowRef<TestGroup[]>([])
const group = shallowRef('')
const loading = shallowRef(false)
const saving = shallowRef(false)
const mustReload = shallowRef(false)
const error = shallowRef('')
const options = computed(() => [
  { value: '', label: '自动创建共享测试分组' },
  ...groups.value.map(item => ({ value: item.id, label: `${item.name}${item.enabled ? '' : '（已停用）'}`, disabled: !item.enabled })),
  ...(group.value && !groups.value.some(item => item.id === group.value)
    ? [
        { value: group.value, label: '原分组已删除，请重新选择', disabled: true },
      ]
    : []),
])
let generation = 0
let alive = true
watch(open, (visible) => {
  if (visible)
    void load()
  else
    generation++
})
onBeforeUnmount(() => {
  alive = false
  generation++
})
async function load() {
  const current = ++generation
  loading.value = true
  settings.value = undefined
  error.value = ''
  mustReload.value = false
  try {
    const value = await getTestSettings()
    if (alive && current === generation) {
      settings.value = value.settings
      groups.value = value.groups
      group.value = settings.value.groupId ?? ''
    }
  }
  catch (cause) {
    if (alive && current === generation) {
      error.value = cause instanceof Error
        ? cause.message
        : '测试设置读取失败'
    }
  }
  finally {
    if (alive && current === generation)
      loading.value = false
  }
}
async function save() {
  if (!settings.value || saving.value || mustReload.value)
    return
  saving.value = true
  error.value = ''
  try {
    await saveTestSettings({ groupId: group.value || null, version: settings.value.version })
    if (alive) {
      emit('saved')
      open.value = false
    }
  }
  catch (cause) {
    if (alive) {
      error.value = cause instanceof Error
        ? cause.message
        : '保存结果待确认，请重新读取'
      mustReload.value = true
    }
  }
  finally {
    if (alive)
      saving.value = false
  }
}
</script>

<template>
  <ZDialog v-model="open" title="测试设置" description="所有账号共用此设置" width="32rem" :show-close="!saving" :close-on-click-modal="false" :close-on-press-escape="!saving">
    <form id="account-headers-test-settings" class="space-y-4" @submit.prevent="save">
      <ZFormItem label="统一测试分组">
        <ZSelect v-model="group" :options="options" aria-label="统一测试分组" :disabled="loading || saving || !settings || mustReload" class="w-full" />
      </ZFormItem>
      <p class="text-z-sm text-z-text-secondary">
        选择已有分组后，请确保它包含所有待测账号。测试会定向选择你点中的账号
      </p>
      <p class="text-z-sm text-z-text-secondary">
        留空时，首次测试自动创建一个共享分组。后续测试复用该组，并补入当前全部账号
      </p>
      <p class="text-z-xs text-z-text-secondary">
        每个测试分组复用一个专用 Key，模型用量计入该 Key
      </p>
      <p v-if="loading" role="status" class="text-z-sm text-z-text-secondary">
        正在读取测试设置
      </p>
      <div v-if="error" role="alert" class="space-y-2 rounded-z bg-z-error-container p-3 text-z-error-on-container">
        <p>{{ error }}</p>
        <ZButton size="small" :disabled="loading || saving" @click="load">
          重新读取
        </ZButton>
      </div>
    </form>
    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        取消
      </ZButton>
      <ZButton type="primary" native-type="submit" form="account-headers-test-settings" :loading="saving" :disabled="loading || !settings || mustReload">
        保存
      </ZButton>
    </template>
  </ZDialog>
</template>
