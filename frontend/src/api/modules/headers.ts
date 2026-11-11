import { isObject, request } from '../request'

/** 宿主已有账号的最小展示事实 */
export interface Account {
  account_id: string
  provider_id: string
  name: string
  email: string | null
  enabled: boolean
  override_enabled: boolean
  header_count: number
}

/** 使用稳定账号游标的列表页 */
export interface AccountPage { accounts: Account[], next_cursor: string | null }

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
 * 查询一页账号及当前覆写状态，搜索覆盖全部账号
 * @param cursor 上一页返回的稳定账号游标
 * @param search 账号名称、邮箱或 ID
 * @return 当前页与下一页游标
 */
export async function listAccounts(cursor: string | null, search: string): Promise<AccountPage> {
  const query = new URLSearchParams({ search })
  if (cursor)
    query.set('cursor', cursor)
  const value = await request('accounts', query)
  if (!isObject(value) || !Array.isArray(value.accounts) || !value.accounts.every(isAccount)
    || (value.next_cursor !== null && typeof value.next_cursor !== 'string')) {
    throw new Error('账号列表响应无效')
  }
  return value as unknown as AccountPage
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
    && typeof value.override_enabled === 'boolean' && Number.isSafeInteger(value.header_count) && Number(value.header_count) >= 0
}

function snapshot(value: unknown, accountId: string): RuleSnapshot {
  if (!isObject(value) || value.accountId !== accountId || typeof value.enabled !== 'boolean'
    || (value.version !== null && (typeof value.version !== 'string' || !/^\d+$/.test(value.version)))
    || !Array.isArray(value.headers) || !value.headers.every(header => isObject(header) && typeof header.name === 'string' && typeof header.value === 'string')) {
    throw new Error('请求头规则响应无效')
  }
  return value as unknown as RuleSnapshot
}
