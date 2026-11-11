import { isObject, request } from '../request'

/** 宿主已有账号的最小展示事实 */
export interface Account {
  account_id: string
  provider_id: string
  name: string
  email: string | null
  enabled: boolean
}

/** 按名称覆写的头值，空字符串表示发送空值 */
export interface HeaderOverride { name: string, value: string }

/** 一个账号的规则与精确持久化版本 */
export interface RuleSnapshot {
  accountId: string
  version: string | null
  enabled: boolean
  headers: HeaderOverride[]
}

/**
 * 遍历宿主账号分页，页面销毁或刷新后停止读取后续页
 * @param signal 当前加载操作的取消信号
 * @return 全部账号事实
 */
export async function listAccounts(signal: AbortSignal): Promise<Account[]> {
  const accounts: Account[] = []
  let cursor: string | null = null
  do {
    signal.throwIfAborted()
    const value = await request('accounts', new URLSearchParams(cursor
      ? { cursor }
      : {}))
    signal.throwIfAborted()
    if (!isObject(value) || !Array.isArray(value.accounts) || !value.accounts.every(isAccount)
      || (value.next_cursor !== null && typeof value.next_cursor !== 'string')) {
      throw new Error('账号列表响应无效')
    }
    accounts.push(...value.accounts)
    cursor = value.next_cursor as string | null
  } while (cursor !== null)
  return accounts
}

/**
 * 读取所选账号的当前规则
 * @param accountId 账号标识
 * @return 可编辑的完整快照
 */
export async function getRule(accountId: string): Promise<RuleSnapshot> {
  return snapshot(await request('rule', new URLSearchParams({ accountId })), accountId)
}

/**
 * 提交完整规则，保留并发版本以避免覆盖其他页面
 * @param input 当前页面的完整草稿
 * @return 宿主确认保存的规范快照
 */
export async function saveRule(input: RuleSnapshot): Promise<RuleSnapshot> {
  return snapshot(await request('rule', new URLSearchParams(), input), input.accountId)
}

function isAccount(value: unknown): value is Account {
  return isObject(value) && ['account_id', 'provider_id', 'name'].every(key => typeof value[key] === 'string')
    && (value.email === null || typeof value.email === 'string') && typeof value.enabled === 'boolean'
}

function snapshot(value: unknown, accountId: string): RuleSnapshot {
  if (!isObject(value) || value.accountId !== accountId || typeof value.enabled !== 'boolean'
    || (value.version !== null && (typeof value.version !== 'string' || !/^\d+$/.test(value.version)))
    || !Array.isArray(value.headers) || !value.headers.every(header => isObject(header) && typeof header.name === 'string' && typeof header.value === 'string')) {
    throw new Error('请求头规则响应无效')
  }
  return value as unknown as RuleSnapshot
}
