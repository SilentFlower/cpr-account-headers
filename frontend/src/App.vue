<script setup lang="ts">
import type { Account, RuleSnapshot } from './api/modules/headers'
import { ZButton, ZCard, ZConfigProvider, ZEmpty, ZInput, ZSelect, ZSwitch } from '@codex-proxy/ui'
import { zhHans } from '@codex-proxy/ui/locale'
import { computed, onBeforeUnmount, ref, shallowRef, watch } from 'vue'
import { getRule, listAccounts, saveRule } from './api/modules/headers'
import { ResponseError } from './api/request'

const accounts = shallowRef<Account[]>([])
const selected = shallowRef<string>()
const saved = shallowRef<RuleSnapshot>()
const draft = ref<RuleSnapshot>()
const loadingAccounts = shallowRef(false)
const loadingRule = shallowRef(false)
const saving = shallowRef(false)
const accountError = shallowRef('')
const error = shallowRef('')
const notice = shallowRef('')
const mustReload = shallowRef(false)
const dirty = computed(() => JSON.stringify(draft.value) !== JSON.stringify(saved.value))
const busy = computed(() => saving.value || loadingRule.value)
const editorDisabled = computed(() => busy.value || mustReload.value)
const options = computed(() => accounts.value.map(account => ({
  value: account.account_id,
  label: `${account.name || account.email || account.account_id} · ${account.provider_id}${account.enabled ? '' : ' · 已停用'}`,
  description: [account.email, account.account_id].filter(Boolean).join(' · '),
})))
let accountController: AbortController | undefined
let ruleSequence = 0
let active = true

onBeforeUnmount(() => {
  active = false
  accountController?.abort()
  ruleSequence++
})
watch(selected, () => void loadRule())
void loadAccounts()

async function loadAccounts() {
  accountController?.abort()
  const controller = new AbortController()
  accountController = controller
  loadingAccounts.value = true
  accountError.value = ''
  try {
    const value = await listAccounts(controller.signal)
    if (!controller.signal.aborted)
      accounts.value = value
  }
  catch (cause) {
    if (!controller.signal.aborted)
      accountError.value = message(cause)
  }
  finally {
    if (!controller.signal.aborted)
      loadingAccounts.value = false
  }
}

async function loadRule() {
  const id = selected.value
  const sequence = ++ruleSequence
  draft.value = undefined
  saved.value = undefined
  error.value = ''
  notice.value = ''
  mustReload.value = false
  if (!id)
    return
  loadingRule.value = true
  try {
    const value = await getRule(id)
    if (active && sequence === ruleSequence) {
      saved.value = value
      draft.value = structuredClone(value)
    }
  }
  catch (cause) {
    if (active && sequence === ruleSequence)
      error.value = message(cause)
  }
  finally {
    if (active && sequence === ruleSequence)
      loadingRule.value = false
  }
}

async function save() {
  if (!draft.value || editorDisabled.value || !dirty.value)
    return
  saving.value = true
  error.value = ''
  notice.value = ''
  try {
    const value = await saveRule(structuredClone({
      ...draft.value,
      headers: draft.value.headers.map(header => ({ ...header })),
    }))
    if (active) {
      saved.value = value
      draft.value = structuredClone(value)
      notice.value = '规则已保存'
    }
  }
  catch (cause) {
    if (active) {
      const rejected = cause instanceof ResponseError && [400, 404, 409].includes(cause.status)
      mustReload.value = !(cause instanceof ResponseError && cause.status === 400)
      error.value = rejected
        ? message(cause)
        : '保存结果待确认，请重新读取当前规则'
    }
  }
  finally {
    if (active)
      saving.value = false
  }
}

function reset() {
  if (saved.value)
    draft.value = structuredClone(saved.value)
  error.value = ''
  notice.value = ''
}

function message(cause: unknown) {
  return cause instanceof Error
    ? cause.message
    : '请求失败'
}
</script>

<template>
  <ZConfigProvider :locale="zhHans">
    <main class="space-y-4 text-z-text">
      <ZCard>
        <div class="flex flex-wrap items-center gap-3">
          <span id="account-label" class="text-z-sm font-emphasis">账号</span>
          <ZSelect v-model="selected" aria-labelledby="account-label" :options="options" filterable placeholder="搜索并选择账号" :loading="loadingAccounts" :disabled="busy || dirty" class="min-w-0 flex-1 sm:max-w-xl" />
          <ZButton :loading="loadingAccounts" :disabled="busy" @click="loadAccounts">
            刷新账号
          </ZButton>
        </div>
        <p v-if="accountError" role="alert" class="mt-3 text-z-sm text-z-error">
          {{ accountError }}
        </p>
        <p v-if="dirty" role="status" class="mt-3 text-z-sm text-z-text-secondary">
          当前账号有未保存的修改，保存或撤销后可切换账号
        </p>
      </ZCard>
      <ZCard>
        <div v-if="error" role="alert" class="mb-4 flex flex-wrap items-center gap-3 rounded-z bg-z-error-container p-3 text-z-error-on-container">
          <span>{{ error }}</span>
          <ZButton v-if="selected && (!draft || mustReload)" size="small" :disabled="busy" @click="loadRule">
            重新读取
          </ZButton>
        </div>
        <p v-if="notice" role="status" class="mb-4 text-z-sm text-z-success">
          {{ notice }}
        </p>
        <p v-if="loadingRule" role="status" class="text-z-sm text-z-text-secondary">
          正在读取请求头规则
        </p>
        <form v-else-if="draft" class="space-y-4" @submit.prevent="save">
          <div class="flex flex-wrap items-center justify-between gap-3">
            <ZSwitch v-model="draft.enabled" label="启用覆写" :disabled="editorDisabled" />
            <span class="text-z-sm text-z-text-secondary">{{ draft.headers.length }} / 64 个请求头</span>
          </div>
          <p class="text-z-sm text-z-text-secondary">
            同名请求头会被替换，名称不区分大小写，其他请求头保持原样
          </p>
          <p class="text-z-sm text-z-text-secondary">
            WebSocket 已建立的连接沿用原握手头，新建连接时使用当前规则
          </p>
          <div v-for="(header, index) in draft.headers" :key="index" class="grid grid-cols-1 items-start gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,2fr)_auto]">
            <ZInput v-model="header.name" :aria-label="`请求头名称 ${index + 1}`" placeholder="请求头名称，如 User-Agent" :disabled="editorDisabled" autocomplete="off" spellcheck="false" />
            <ZInput v-model="header.value" :aria-label="`请求头值 ${index + 1}`" placeholder="请求头值，可留空" type="password" show-password :disabled="editorDisabled" autocomplete="off" spellcheck="false" />
            <ZButton :aria-label="`移除请求头 ${index + 1}`" :disabled="editorDisabled" @click="draft.headers.splice(index, 1)">
              移除
            </ZButton>
          </div>
          <ZEmpty v-if="!draft.headers.length" title="尚未配置请求头" description="添加需要覆写的名称和值" surface="none" />
          <div class="flex flex-wrap items-center gap-3">
            <ZButton :disabled="editorDisabled || draft.headers.length >= 64" @click="draft.headers.push({ name: '', value: '' })">
              添加请求头
            </ZButton>
            <ZButton :disabled="editorDisabled || !dirty" @click="reset">
              撤销修改
            </ZButton>
            <ZButton type="primary" native-type="submit" :loading="saving" :disabled="editorDisabled || !dirty" class="sm:ml-auto">
              保存
            </ZButton>
          </div>
        </form>
        <ZEmpty v-else-if="!selected" :title="loadingAccounts ? '正在读取账号' : accounts.length ? '请选择账号' : '暂无账号'" description="每个账号单独保存请求头规则" surface="none" />
      </ZCard>
    </main>
  </ZConfigProvider>
</template>
