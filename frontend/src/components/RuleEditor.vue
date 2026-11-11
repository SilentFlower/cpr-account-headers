<script setup lang="ts">
import type { Account, RuleSnapshot } from '../api/modules/headers'
import { ZButton, ZDialog, ZEmpty, ZInput, ZSwitch } from '@codex-proxy/ui'
import { computed, onBeforeUnmount, ref, shallowRef, watch } from 'vue'
import { getRule, saveRule } from '../api/modules/headers'
import { ResponseError } from '../api/request'

const props = defineProps<{ account?: Account }>()
const emit = defineEmits<{ saved: [snapshot: RuleSnapshot] }>()
const open = defineModel<boolean>({ required: true })
const saved = shallowRef<RuleSnapshot>()
const draft = ref<RuleSnapshot>()
const loading = shallowRef(false)
const saving = shallowRef(false)
const error = shallowRef('')
const mustReload = shallowRef(false)
const dirty = computed(() => JSON.stringify(draft.value) !== JSON.stringify(saved.value))
const disabled = computed(() => loading.value || saving.value || mustReload.value)
let generation = 0
let alive = true
onBeforeUnmount(() => {
  alive = false
  generation++
})
watch([
  open,
  () => props.account?.account_id,
], ([visible]) => {
  if (visible) {
    void load()
  }
  else {
    generation++
    draft.value = undefined
    saved.value = undefined
    error.value = ''
  }
})
async function load() {
  const id = props.account?.account_id
  if (!id)
    return
  const current = ++generation
  draft.value = undefined
  saved.value = undefined
  error.value = ''
  mustReload.value = false
  loading.value = true
  try {
    const value = await getRule(id)
    if (alive && current === generation) {
      saved.value = value
      draft.value = structuredClone(value)
    }
  }
  catch (cause) {
    if (alive && current === generation) {
      error.value = cause instanceof Error
        ? cause.message
        : '请求头规则读取失败'
    }
  }
  finally {
    if (alive && current === generation)
      loading.value = false
  }
}
async function save() {
  if (!draft.value || disabled.value || !dirty.value)
    return
  saving.value = true
  error.value = ''
  try {
    const value = await saveRule({
      ...draft.value,
      headers: draft.value.headers.map(header => ({ ...header })),
    })
    if (alive) {
      emit('saved', value)
      open.value = false
    }
  }
  catch (cause) {
    if (alive) {
      const rejected = cause instanceof ResponseError && [400, 404, 409].includes(cause.status)
      mustReload.value = !(cause instanceof ResponseError && cause.status === 400)
      error.value = rejected && cause instanceof Error
        ? cause.message
        : '保存结果待确认，请重新读取当前规则'
    }
  }
  finally {
    if (alive)
      saving.value = false
  }
}
</script>

<template>
  <ZDialog v-model="open" title="编辑请求头" :description="account?.name || account?.email || account?.account_id" width="52rem" :show-close="!saving" :close-on-click-modal="false" :close-on-press-escape="!saving">
    <form id="account-headers-editor" class="space-y-4" @submit.prevent="save">
      <p v-if="loading" role="status" class="text-z-sm text-z-text-secondary">
        正在读取请求头规则
      </p>
      <div v-if="error" role="alert" class="flex flex-wrap items-center gap-3 rounded-z bg-z-error-container p-3 text-z-error-on-container">
        <span>{{ error }}</span>
        <ZButton v-if="!draft || mustReload" size="small" :disabled="loading || saving" @click="load">
          重新读取
        </ZButton>
      </div>
      <template v-if="draft">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <ZSwitch v-model="draft.enabled" label="启用覆写" :disabled="disabled" />
          <span class="text-z-sm text-z-text-secondary">{{ draft.headers.length }} / 64 个请求头</span>
        </div>
        <p class="text-z-sm text-z-text-secondary">
          同名请求头会被替换，名称不区分大小写，头值直接显示
        </p>
        <p class="text-z-sm text-z-text-secondary">
          WebSocket 已建立的连接沿用原握手头，新建连接时使用当前规则
        </p>
        <div v-for="(header, index) in draft.headers" :key="index" class="grid grid-cols-1 items-start gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,2fr)_auto]">
          <ZInput v-model="header.name" :aria-label="`请求头名称 ${index + 1}`" placeholder="请求头名称，如 User-Agent" :disabled="disabled" autocomplete="off" spellcheck="false" />
          <ZInput v-model="header.value" :aria-label="`请求头值 ${index + 1}`" placeholder="请求头值，可留空" :disabled="disabled" autocomplete="off" spellcheck="false" />
          <ZButton :aria-label="`移除请求头 ${index + 1}`" :disabled="disabled" @click="draft.headers.splice(index, 1)">
            移除
          </ZButton>
        </div>
        <ZEmpty v-if="!draft.headers.length" title="尚未配置请求头" surface="none" />
        <ZButton :disabled="disabled || draft.headers.length >= 64" @click="draft.headers.push({ name: '', value: '' })">
          添加请求头
        </ZButton>
      </template>
    </form>
    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        取消
      </ZButton>
      <ZButton type="primary" native-type="submit" form="account-headers-editor" :loading="saving" :disabled="disabled || !draft || !dirty">
        保存
      </ZButton>
    </template>
  </ZDialog>
</template>
