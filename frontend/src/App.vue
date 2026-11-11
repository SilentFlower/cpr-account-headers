<script setup lang="ts">
import type { TableColumn } from '@codex-proxy/ui'
import type { Account, AccountPage, RuleSnapshot } from './api/modules/headers'
import { ZButton, ZCard, ZConfigProvider, ZEmpty, ZInput, ZTable, ZTag } from '@codex-proxy/ui'
import { zhHans } from '@codex-proxy/ui/locale'
import { computed, onBeforeUnmount, shallowRef } from 'vue'
import { listAccounts } from './api/modules/headers'
import AccountTest from './components/AccountTest.vue'
import RuleEditor from './components/RuleEditor.vue'

const snapshot = shallowRef<AccountPage>()
const loading = shallowRef(false)
const error = shallowRef('')
const search = shallowRef('')
const appliedSearch = shallowRef('')
const cursors = shallowRef<(string | null)[]>([null])
const page = shallowRef(0)
const selected = shallowRef<Account>()
const editorOpen = shallowRef(false)
const testerOpen = shallowRef(false)
const modalOpen = computed(() => editorOpen.value || testerOpen.value)
const notice = shallowRef('')
const rows = computed(() => snapshot.value?.accounts ?? [])
const columns: TableColumn<Account>[] = [
  {
    key: 'name',
    label: '账号',
    kind: 'identity',
    minWidth: 260,
  },
  { key: 'provider_id', label: '平台', minWidth: 90 },
  {
    key: 'enabled',
    label: '账号状态',
    kind: 'status',
    minWidth: 100,
  },
  {
    key: 'override_enabled',
    label: '请求头覆写',
    kind: 'status',
    minWidth: 110,
  },
  {
    key: 'header_count',
    label: '请求头数量',
    kind: 'numeric',
    minWidth: 100,
  },
  {
    key: 'actions',
    label: '操作',
    kind: 'actions',
    width: 140,
  },
]
let sequence = 0
let alive = true
onBeforeUnmount(() => {
  alive = false
  sequence++
})
void load(0, '', [null])

async function load(target: number, query = appliedSearch.value, history = cursors.value) {
  const current = ++sequence
  loading.value = true
  error.value = ''
  try {
    const value = await listAccounts(history[target] ?? null, query)
    if (alive && current === sequence) {
      snapshot.value = value
      page.value = target
      cursors.value = history
      appliedSearch.value = query
    }
  }
  catch (cause) {
    if (alive && current === sequence) {
      error.value = cause instanceof Error
        ? cause.message
        : '账号列表读取失败'
    }
  }
  finally {
    if (alive && current === sequence)
      loading.value = false
  }
}
function query() {
  void load(0, search.value.trim(), [null])
}
function next() {
  if (snapshot.value?.next_cursor) {
    void load(page.value + 1, appliedSearch.value, [
      ...cursors.value.slice(0, page.value + 1),
      snapshot.value.next_cursor,
    ])
  }
}
function edit(account: Account) {
  selected.value = account
  editorOpen.value = true
  notice.value = ''
}
function test(account: Account) {
  selected.value = account
  testerOpen.value = true
  notice.value = ''
}
function saved(value: RuleSnapshot) {
  if (snapshot.value) {
    snapshot.value = {
      ...snapshot.value,
      accounts: snapshot.value.accounts.map(account => account.account_id === value.accountId
        ? { ...account, override_enabled: value.enabled, header_count: value.headers.length }
        : account),
    }
  }
  notice.value = '规则已保存'
}
</script>

<template>
  <ZConfigProvider :locale="zhHans">
    <main class="space-y-4 text-z-text" :class="{ 'overflow-clip': modalOpen }" :style="modalOpen ? { maxHeight: 'var(--plugin-viewport-height, 100dvh)' } : undefined">
      <RuleEditor v-model="editorOpen" :account="selected" @saved="saved" />
      <AccountTest v-model="testerOpen" :account="selected" />
      <ZCard>
        <form class="flex flex-wrap items-center gap-3" @submit.prevent="query">
          <ZInput v-model="search" aria-label="搜索账号" placeholder="账号名称、邮箱或 ID" maxlength="256" class="w-full sm:max-w-96" />
          <ZButton type="primary" native-type="submit" :disabled="loading || modalOpen">
            查询
          </ZButton>
          <ZButton :loading="loading" :disabled="modalOpen" class="sm:ml-auto" @click="load(page)">
            刷新
          </ZButton>
        </form>
        <p class="mt-3 text-z-sm text-z-text-secondary">
          点击“测试”可查看此账号在普通模型链路中覆写前后的请求头
        </p>
      </ZCard>
      <ZCard>
        <div v-if="error" role="alert" class="mb-4 flex flex-wrap items-center gap-3 rounded-z bg-z-error-container p-3 text-z-error-on-container">
          <span>{{ error }}</span>
          <ZButton size="small" :disabled="loading" @click="load(page)">
            重新读取
          </ZButton>
        </div>
        <p v-if="notice" role="status" class="mb-4 text-z-sm text-z-success">
          {{ notice }}
        </p>
        <ZTable :columns="columns" :data="rows" row-key="account_id" :loading="loading" size="small">
          <template #name="{ row }">
            <div class="space-y-1">
              <div class="truncate font-emphasis" :title="row.name || row.account_id">
                {{ row.name || row.account_id }}
              </div>
              <div class="truncate text-z-xs text-z-text-secondary" :title="row.email || row.account_id">
                {{ row.email || row.account_id }}
              </div>
            </div>
          </template>
          <template #enabled="{ row }">
            <ZTag :type="row.enabled ? 'success' : 'info'" size="small">
              {{ row.enabled ? '已启用' : '已停用' }}
            </ZTag>
          </template>
          <template #override_enabled="{ row }">
            <ZTag :type="row.override_enabled ? 'success' : 'info'" size="small">
              {{ row.override_enabled ? '已开启' : '已关闭' }}
            </ZTag>
          </template>
          <template #actions="{ row }">
            <ZButton variant="link" size="small" :aria-label="`编辑 ${row.name || row.account_id}`" :disabled="loading || modalOpen" @click="edit(row)">
              编辑
            </ZButton>
            <ZButton variant="link" size="small" :aria-label="`测试 ${row.name || row.account_id}`" :disabled="loading || modalOpen" @click="test(row)">
              测试
            </ZButton>
          </template>
          <template #empty>
            <ZEmpty :title="error ? '账号读取失败' : '暂无匹配账号'" surface="none" />
          </template>
        </ZTable>
        <div v-if="snapshot" class="mt-4 flex flex-wrap items-center justify-between gap-3 text-z-sm text-z-text-secondary">
          <span>第 {{ page + 1 }} 页，本页 {{ rows.length }} 个账号</span>
          <div class="flex gap-2">
            <ZButton size="small" :disabled="loading || modalOpen || page === 0" @click="load(page - 1)">
              上一页
            </ZButton>
            <ZButton size="small" :disabled="loading || modalOpen || !snapshot.next_cursor" @click="next">
              下一页
            </ZButton>
          </div>
        </div>
      </ZCard>
    </main>
  </ZConfigProvider>
</template>
