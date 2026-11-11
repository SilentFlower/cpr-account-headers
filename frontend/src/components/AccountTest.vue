<script setup lang="ts">
import type { Account } from '../api/modules/headers'
import type { OutboundEvidence, TestResult } from '../api/modules/testing'
import { ZButton, ZDialog, ZFormItem, ZInput, ZTag } from '@codex-proxy/ui'
import { onBeforeUnmount, shallowRef, watch } from 'vue'
import { testAccount } from '../api/modules/testing'

const props = defineProps<{ account?: Account }>()
const open = defineModel<boolean>({ required: true })
const model = shallowRef('')
const active = shallowRef('')
const stopping = shallowRef(false)
const result = shallowRef<TestResult & { elapsedMs: number } | { status: 'cancelled' | 'timeout', elapsedMs: number, outbound: OutboundEvidence[] }>()
let controller: AbortController | undefined
const error = shallowRef('')
let generation = 0
let alive = true
const labels: Record<TestResult['status'] | 'cancelled' | 'timeout', string> = {
  success: '模型请求成功',
  failed: '测试失败',
  cancelled: '测试已停止',
  timeout: '测试超时',
  unverified: '尚未验证覆写',
}
watch(open, (visible) => {
  if (visible) {
    result.value = undefined
    error.value = ''
  }
  else {
    void stop()
    generation++
  }
})
onBeforeUnmount(() => {
  void stop()
  generation++
  alive = false
})
async function run() {
  const accountId = props.account?.account_id
  if (!accountId || active.value || !model.value.trim())
    return
  const current = ++generation
  const id = Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, '0')).join('')
  active.value = id
  const started = performance.now()
  const currentController = new AbortController()
  controller = currentController
  let timeout = false
  const timer = window.setTimeout(() => {
    timeout = true
    currentController.abort()
  }, 45000)
  error.value = ''
  result.value = undefined
  try {
    const value = await testAccount(id, accountId, model.value.trim(), currentController.signal)
    if (alive && current === generation)
      result.value = { ...value, elapsedMs: performance.now() - started }
  }
  catch (cause) {
    if (alive && current === generation) {
      if (currentController.signal.aborted) {
        result.value = {
          status: timeout ? 'timeout' : 'cancelled',
          elapsedMs: performance.now() - started,
          outbound: [],
        }
      }
      else {
        error.value = cause instanceof Error
          ? cause.message
          : '测试请求失败'
      }
    }
  }
  finally {
    window.clearTimeout(timer)
    if (controller === currentController)
      controller = undefined
    if (alive && active.value === id) {
      active.value = ''
      stopping.value = false
    }
  }
}
function stop() {
  if (!active.value || stopping.value)
    return
  stopping.value = true
  controller?.abort()
}
</script>

<template>
  <ZDialog v-model="open" title="测试请求头" :description="[account?.name || account?.account_id, account?.email && account.email !== account.name ? account.email : null].filter(Boolean).join(' · ')" width="52rem" :close-on-click-modal="false">
    <form id="account-headers-test" class="space-y-4" @submit.prevent="run">
      <p class="text-z-sm text-z-text-secondary">
        使用统一测试分组和专用 Key，定向测试当前账号，最长等待 45 秒
      </p>
      <p class="text-z-xs text-z-text-secondary">
        可在账号列表的「测试设置」中选择已有分组。留空时自动创建并复用共享分组
      </p>
      <ZFormItem label="上游模型 ID" for="probe-model">
        <ZInput id="probe-model" v-model="model" aria-label="上游模型 ID" placeholder="填写此账号支持的模型 ID" :disabled="!!active" maxlength="256" autocomplete="off" />
      </ZFormItem>
      <p v-if="active" role="status" class="text-z-sm text-z-text-secondary">
        {{ stopping ? '正在停止测试' : '正在请求上游' }}
      </p>
      <p v-if="error" role="alert" class="rounded-z bg-z-error-container p-3 text-z-error-on-container">
        {{ error }}
      </p>
      <div v-if="result" class="space-y-4" role="status">
        <div class="flex flex-wrap items-center gap-3">
          <ZTag :type="result.status === 'success' ? 'success' : 'info'">
            {{ labels[result.status] }}
          </ZTag>
          <span class="text-z-sm text-z-text-secondary">{{ (result.elapsedMs / 1000).toFixed(2) }} 秒</span>
        </div>
        <p v-if="'message' in result && result.message" class="whitespace-pre-wrap break-words text-z-sm">
          {{ result.message }}
        </p>
        <div v-for="(outbound, index) in result.outbound" :key="index" class="space-y-3 rounded-z bg-z-fill-alter p-4">
          <div class="flex flex-wrap items-center justify-between gap-2 text-z-sm">
            <span>本插件覆写后的请求头</span>
            <ZTag size="small" type="info">
              {{ outbound.transport }}
            </ZTag>
          </div>
          <p class="break-all text-z-xs text-z-text-secondary">
            账号 {{ outbound.accountId }}
          </p>
          <p class="text-z-xs text-z-text-secondary">
            第 {{ outbound.attemptIndex }} 次准备 · {{ outbound.overrideEnabled ? '覆写已开启' : '覆写已关闭' }}
          </p>
          <dl class="space-y-3 text-z-sm">
            <div v-for="name in Array.from(new Set(['user-agent', 'originator', ...outbound.headers.map(header => header.name.toLowerCase())]))" :key="name" class="space-y-1">
              <dt class="text-z-text-secondary">
                {{ name }}
              </dt>
              <dd class="m-0 whitespace-pre-wrap break-all font-mono">
                {{ outbound.headers.filter(header => header.name.toLowerCase() === name).map(header => header.value === '' ? '（空值）' : header.value).join('\n') || '（未设置）' }}
              </dd>
            </div>
          </dl>
          <details class="text-z-sm">
            <summary class="cursor-pointer">
              覆写前的身份头
            </summary>
            <dl class="mt-2 space-y-2">
              <div v-for="header in outbound.beforeHeaders" :key="header.name">
                <dt class="text-z-text-secondary">
                  {{ header.name }}
                </dt>
                <dd class="m-0 break-all font-mono">
                  {{ header.value || '（空值）' }}
                </dd>
              </div>
            </dl>
          </details>
        </div>
        <pre v-if="'text' in result && result.text" class="whitespace-pre-wrap break-words text-z-sm">{{ result.text }}</pre>
        <details v-if="'error' in result && result.error" class="text-z-sm">
          <summary class="cursor-pointer">
            模型接口响应（HTTP {{ result.error.upstreamStatus }}）
          </summary>
          <pre class="mt-2 whitespace-pre-wrap break-all">{{ result.error.upstreamBody }}</pre>
        </details>
      </div>
    </form>
    <template #footer>
      <ZButton @click="open = false">
        关闭
      </ZButton>
      <ZButton v-if="active" :loading="stopping" @click="stop">
        停止测试
      </ZButton>
      <ZButton v-else type="primary" native-type="submit" form="account-headers-test" :disabled="!model.trim()">
        开始测试
      </ZButton>
    </template>
  </ZDialog>
</template>
