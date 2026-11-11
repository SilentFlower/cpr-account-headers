// @env browser

/** 固定到当前实例与制品的宿主桥 */
interface PluginBridge {
  readonly version: 2
  request: (input: { method: 'GET' | 'POST', path: string, query: string, contentType?: string, body?: string }) => Promise<{ status: number, contentType: string, body: ArrayBuffer }>
}

declare global {
  /** 隔离页面只通过宿主桥访问管理路由 */
  interface Window { readonly codexProxyPlugin?: PluginBridge }
}

/** 保留明确拒绝状态，供页面区分写入冲突与结果未知 */
export class ResponseError extends Error {
  /**
   * 创建安全的业务错误
   * @param status 管理路由状态
   * @param message 安全中文提示
   */
  constructor(readonly status: number, message: string) { super(message) }
}

/**
 * 判断响应中的 JSON 对象
 * @param value 待检查数据
 * @return 是否为普通对象
 */
export function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * 调用当前插件的已注册路由，不自动重放写入
 * @param path 相对管理路径
 * @param query 查询参数
 * @param body POST 正文，省略时使用 GET
 * @return 原始业务 JSON
 */
export async function request(path: string, query = new URLSearchParams(), body?: unknown): Promise<unknown> {
  const bridge = window.codexProxyPlugin
  if (!bridge || bridge.version !== 2)
    throw new Error('请从 CPR 的插件扩展入口打开此页')
  const response = await bridge.request({
    method: body === undefined
      ? 'GET'
      : 'POST',
    path,
    query: query.toString(),
    ...(body === undefined
      ? {}
      : {
          contentType: 'application/json',
          body: JSON.stringify(body),
        }),
  })
  if (response.contentType.split(';', 1)[0]?.trim().toLowerCase() !== 'application/json')
    throw new Error('插件返回了不支持的内容类型')
  let value: unknown
  try {
    value = JSON.parse(new TextDecoder().decode(response.body))
  }
  catch {
    throw new Error('插件返回了无效 JSON')
  }
  if (response.status < 200 || response.status >= 300) {
    const message = isObject(value) && isObject(value.error) && typeof value.error.message === 'string'
      ? value.error.message
      : `插件请求失败（HTTP ${response.status}）`
    throw new ResponseError(response.status, message)
  }
  return value
}
