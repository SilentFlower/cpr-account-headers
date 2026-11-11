import type { HeaderOverride } from './headers'
import { isObject, request } from '../request'

/** 本插件真实出站处理器记录的请求头前后对照 */
export interface OutboundEvidence {
  accountId: string
  requestId: string
  attemptIndex: number
  transport: string
  overrideEnabled: boolean
  beforeHeaders: HeaderOverride[]
  headers: HeaderOverride[]
}

/** 普通模型请求的真实结果与同一请求的插件记录 */
export interface TestResult {
  status: 'success' | 'failed' | 'unverified'
  message?: string
  text?: string
  outbound: OutboundEvidence[]
  error?: { upstreamStatus: number, upstreamBody: string }
}

/**
 * 使用专用单账号 Key 从页面模型桥发请求，当前插件按普通链路参与
 * @param id 随机测试编号
 * @param accountId 所选账号
 * @param modelId 模型标识
 * @param signal 页面取消与期限
 * @return 模型结果及按网关请求 ID 匹配的覆写记录
 */
export async function testAccount(id: string, accountId: string, modelId: string, signal: AbortSignal): Promise<TestResult> {
  try {
    const plan = await abortable(request('test', new URLSearchParams(), { id, accountId, modelId }), signal)
    if (!isObject(plan) || plan.id !== id || plan.accountId !== accountId || typeof plan.clientKeyId !== 'string')
      throw new Error('测试身份响应无效')
    const bridge = window.codexProxyPlugin
    if (!bridge?.models || typeof bridge.models.responses !== 'function')
      throw new Error('当前页面缺少模型桥，请从宿主插件扩展入口打开')
    const response = await bridge.models.responses({
      clientKeyId: plan.clientKeyId,
      body: {
        model: modelId,
        input: 'Reply with exactly OK.',
        stream: true,
        store: false,
      },
      signal,
    })
    const raw = await readBody(response, signal)
    let value: unknown
    try {
      value = parseResponse(raw, response.headers.get('content-type') ?? '')
    }
    catch { throw new Error('模型接口返回了无效响应') }
    const requestId = response.headers.get('x-gateway-request-id')
    let outbound: OutboundEvidence[] = []
    if (requestId) {
      const result = await abortable(request('test', new URLSearchParams({ id, requestId })), signal)
      if (!isObject(result) || !Array.isArray(result.outbound) || !result.outbound.every(item => isEvidence(item, accountId, requestId)))
        throw new Error('请求头诊断响应无效')
      outbound = result.outbound
    }
    if (!response.ok || !isObject(value) || value.status !== 'completed') {
      const message = isObject(value) && isObject(value.error) && typeof value.error.message === 'string'
        ? value.error.message
        : `模型请求未完成（HTTP ${response.status}）`
      return {
        status: 'failed',
        message,
        outbound,
        error: { upstreamStatus: response.status, upstreamBody: raw },
      }
    }
    if (!requestId || !outbound.length) {
      return { status: 'unverified', message: '未取得本插件的出站记录，请检查绑定是否覆盖测试 Key、分组和模型', outbound }
    }
    const text = Array.isArray(value.output)
      ? value.output.filter(isObject).flatMap(item => Array.isArray(item.content)
          ? item.content
          : []).filter(item => isObject(item) && item.type === 'output_text' && typeof item.text === 'string').map(item => item.text).join('')
      : ''
    return { status: 'success', outbound, text }
  }
  finally {
    // 页面取消模型流，管理调用只结束短期关联，专用分组与 Key 留待下次复用
    await request(signal.aborted ? 'test/cancel' : 'test/finish', new URLSearchParams(), { id }).catch(() => {})
  }
}

function parseResponse(raw: string, contentType: string): unknown {
  if (!contentType.toLowerCase().startsWith('text/event-stream'))
    return JSON.parse(raw)
  let terminal: unknown
  for (const event of raw.replace(/\r\n/g, '\n').split('\n\n')) {
    const data = event.split('\n').filter(line => line.startsWith('data:')).map(line => line.slice(5).trimStart()).join('\n')
    if (!data || data === '[DONE]')
      continue
    const value: unknown = JSON.parse(data)
    if (!isObject(value))
      continue
    if (['response.completed', 'response.incomplete', 'response.failed'].includes(String(value.type))) {
      terminal = value.response
    }
    else if (value.type === 'error') {
      terminal = {
        status: 'failed',
        error: isObject(value.error)
          ? value.error
          : value,
      }
    }
  }
  return terminal
}

function isEvidence(value: unknown, accountId: string, requestId: string): value is OutboundEvidence {
  const headers = (items: unknown) => Array.isArray(items) && items.every(item => isObject(item) && typeof item.name === 'string' && typeof item.value === 'string')
  return isObject(value) && value.accountId === accountId && value.requestId === requestId
    && typeof value.attemptIndex === 'number' && typeof value.transport === 'string' && typeof value.overrideEnabled === 'boolean'
    && headers(value.beforeHeaders) && headers(value.headers)
}

async function abortable<T>(operation: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted)
    throw new DOMException('测试已停止', 'AbortError')
  let onAbort: () => void = () => {}
  const cancelled = new Promise<never>((_, reject) => {
    onAbort = () => reject(new DOMException('测试已停止', 'AbortError'))
    signal.addEventListener('abort', onAbort, { once: true })
  })
  try {
    return await Promise.race([operation, cancelled])
  }
  finally { signal.removeEventListener('abort', onAbort) }
}

async function readBody(response: Response, signal: AbortSignal): Promise<string> {
  const reader = response.body?.getReader()
  if (!reader)
    throw new Error('模型接口响应正文为空')
  const chunks: Uint8Array[] = []
  let length = 0
  let complete = false
  try {
    while (true) {
      const { done, value } = await abortable(reader.read(), signal)
      if (done) {
        complete = true
        break
      }
      length += value.byteLength
      if (length > 512 * 1024)
        throw new Error('测试响应超过 512 KiB')
      chunks.push(value)
    }
  }
  finally {
    if (!complete)
      await reader.cancel().catch(() => {})
    reader.releaseLock()
  }
  const bytes = new Uint8Array(length)
  let offset = 0
  for (const chunk of chunks) {
    bytes.set(chunk, offset)
    offset += chunk.byteLength
  }
  return new TextDecoder().decode(bytes)
}
