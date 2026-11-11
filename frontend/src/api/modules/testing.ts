import type { HeaderOverride } from './headers'
import { isObject, request } from '../request'

/** 宿主完整出站准备链返回后的诊断摘要 */
export interface OutboundEvidence {
  accountId: string
  requestId: string
  transport: string
  headers: HeaderOverride[]
}

/** 单次真实测试的终态，失败时仍可能有准备头 */
export interface TestResult {
  status: 'success' | 'failed' | 'cancelled' | 'timeout' | 'unsupported'
  elapsedMs: number
  message?: string
  text?: string
  outbound?: OutboundEvidence[]
  error?: { sendState?: string | null, upstreamStatus?: number | null, upstreamBody?: string | null }
}

/**
 * 固定所选账号发送一次模型请求，使用已保存的覆写规则
 * @param id 页面生成的随机测试编号
 * @param accountId 所选账号
 * @param modelId 上游模型标识
 * @return 含最终身份头的真实诊断结果
 */
export async function testAccount(id: string, accountId: string, modelId: string): Promise<TestResult> {
  const value = await request('test', new URLSearchParams(), { id, accountId, modelId })
  if (!isObject(value) || ![
    'success',
    'failed',
    'cancelled',
    'timeout',
    'unsupported',
  ].includes(String(value.status))
  || typeof value.elapsedMs !== 'number'
  || (value.message !== undefined && typeof value.message !== 'string')
  || (value.text !== undefined && typeof value.text !== 'string')
  || (value.error !== undefined && !isTestError(value.error))
  || (value.status === 'success' && (!Array.isArray(value.outbound) || value.outbound.length === 0))
  || (value.outbound !== undefined && (!Array.isArray(value.outbound) || !value.outbound.every(item => isObject(item)
    && item.accountId === accountId && typeof item.requestId === 'string' && typeof item.transport === 'string'
    && Array.isArray(item.headers) && item.headers.every(header => isObject(header) && typeof header.name === 'string' && typeof header.value === 'string'))))) {
    throw new Error('测试结果响应无效')
  }
  return value as unknown as TestResult
}

/**
 * 结束当前测试，宿主回收本次子调用资源
 * @param id 当前测试编号
 * @return 取消登记完成
 */
export async function cancelTest(id: string): Promise<void> {
  await request('test/cancel', new URLSearchParams(), { id })
}

function isTestError(value: unknown): boolean {
  return isObject(value)
    && [value.sendState, value.upstreamBody].every(field => field === undefined || field === null || typeof field === 'string')
    && (value.upstreamStatus === undefined || value.upstreamStatus === null || typeof value.upstreamStatus === 'number')
}
