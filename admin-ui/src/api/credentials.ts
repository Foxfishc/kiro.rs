import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  CredentialsStatusResponse,
  BalanceResponse,
  CachedBalancesResponse,
  SuccessResponse,
  SetDisabledRequest,
  SetPriorityRequest,
  SetEndpointRequest,
  SetIdpRequest,
  SetCredentialProxyRequest,
  OverageStatusResponse,
  OverageEvent,
  AddCredentialRequest,
  AddCredentialResponse,
  CredentialStatsResponse,
  CredentialAccountInfoResponse,
  ImportTokenJsonRequest,
  ImportTokenJsonResponse,
  ProxyConfigResponse,
  UpdateProxyConfigRequest,
  GlobalConfigResponse,
  UpdateGlobalConfigRequest,
} from '@/types/api'

// 创建 axios 实例
const api = axios.create({
  baseURL: '/api/admin',
  headers: {
    'Content-Type': 'application/json',
  },
})

// 请求拦截器添加 API Key
api.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) {
    config.headers['x-api-key'] = apiKey
  }
  return config
})

// 获取所有凭据状态
export async function getCredentials(): Promise<CredentialsStatusResponse> {
  const { data } = await api.get<CredentialsStatusResponse>('/credentials')
  return data
}

// 设置凭据禁用状态
export async function setCredentialDisabled(
  id: number,
  disabled: boolean
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/disabled`,
    { disabled } as SetDisabledRequest
  )
  return data
}

// 设置凭据优先级
export async function setCredentialPriority(
  id: number,
  priority: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/priority`,
    { priority } as SetPriorityRequest
  )
  return data
}

// 重置失败计数
export async function resetCredentialFailure(
  id: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/reset`)
  return data
}

// 设置凭据 Region
export async function setCredentialRegion(
  id: number,
  region: string | null,
  apiRegion: string | null
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/region`, {
    region: region || null,
    apiRegion: apiRegion || null,
  })
  return data
}

// 设置凭据 endpoint
export async function setCredentialEndpoint(
  id: number,
  endpoint: string | null
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/endpoint`,
    { endpoint } as SetEndpointRequest
  )
  return data
}

// 设置凭据级 Web Portal Idp
export async function setCredentialIdp(
  id: number,
  idp: string | null
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/idp`,
    { idp } as SetIdpRequest
  )
  return data
}

// 设置凭据级代理
export async function setCredentialProxy(
  id: number,
  req: SetCredentialProxyRequest
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/proxy`, req)
  return data
}

// 读取凭据 overage 状态（不触发开启）
export async function getOverageStatus(id: number): Promise<OverageStatusResponse> {
  const { data } = await api.get<OverageStatusResponse>(`/credentials/${id}/overage`)
  return data
}

/**
 * 开启凭据超额（SSE 流）
 *
 * 用 fetch + ReadableStream 实现，而不是浏览器原生 EventSource——后者不能
 * 自定义请求头，而 admin 中间件只认 `x-api-key` 头 + `Authorization: Bearer`。
 *
 * 服务端是 fire-and-forget：客户端断开（调用返回的 close）不会取消后台任务，
 * 任务跑完后状态会落到 `overage_enabled` / `overage_last_error`，下次打开
 * 对话框可以从 `getOverageStatus` 拉到。
 *
 * 收到 `done` / `error` 后内部会自动 abort，无需调用方再 close；调用方主动
 * close 也是安全的（重复 abort 是幂等的）。
 */
export function openOverageEnableStream(
  id: number,
  onEvent: (event: OverageEvent) => void,
  onError?: (err: unknown) => void
): { close: () => void } {
  // 优先用 fetch + stream，避免依赖 EventSource 的 header 限制
  const controller = new AbortController()
  const apiKey = storage.getApiKey() || ''

  fetch(`/api/admin/credentials/${id}/overage/enable`, {
    method: 'GET',
    headers: {
      Accept: 'text/event-stream',
      'x-api-key': apiKey,
    },
    signal: controller.signal,
  })
    .then(async (resp) => {
      if (!resp.ok || !resp.body) {
        const text = await resp.text().catch(() => '')
        onError?.(new Error(`overage 流打开失败 ${resp.status}: ${text}`))
        return
      }
      const reader = resp.body.getReader()
      const decoder = new TextDecoder('utf-8')
      let buffer = ''
      // 简单 SSE 解析：按 \n\n 分块，取每块以 "data:" 开头的行
      while (true) {
        const { value, done } = await reader.read()
        if (done) break
        buffer += decoder.decode(value, { stream: true })
        let sep
        while ((sep = buffer.indexOf('\n\n')) >= 0) {
          const block = buffer.slice(0, sep)
          buffer = buffer.slice(sep + 2)
          const dataLines = block
            .split('\n')
            .filter((l) => l.startsWith('data:'))
            .map((l) => l.slice(5).trimStart())
          if (dataLines.length === 0) continue
          const payload = dataLines.join('\n')
          try {
            const event = JSON.parse(payload) as OverageEvent
            onEvent(event)
            if (event.kind === 'done' || event.kind === 'error') {
              controller.abort()
              return
            }
          } catch (e) {
            onError?.(e)
          }
        }
      }
    })
    .catch((err) => {
      if ((err as { name?: string })?.name !== 'AbortError') {
        onError?.(err)
      }
    })

  return { close: () => controller.abort() }
}

// 强制刷新 Token
export async function forceRefreshToken(id: number): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/refresh`)
  return data
}

// 获取凭据余额
export async function getCredentialBalance(id: number): Promise<BalanceResponse> {
  const { data } = await api.get<BalanceResponse>(`/credentials/${id}/balance`)
  return data
}

// 获取所有凭据的缓存余额
export async function getCachedBalances(): Promise<CachedBalancesResponse> {
  const { data } = await api.get<CachedBalancesResponse>('/credentials/balances/cached')
  return data
}

// 获取凭据账号信息（套餐/用量/邮箱等）
export async function getCredentialAccountInfo(
  id: number
): Promise<CredentialAccountInfoResponse> {
  const { data } = await api.get<CredentialAccountInfoResponse>(`/credentials/${id}/account`)
  return data
}

// 添加新凭据
export async function addCredential(
  req: AddCredentialRequest
): Promise<AddCredentialResponse> {
  const { data } = await api.post<AddCredentialResponse>('/credentials', req)
  return data
}

// 删除凭据
export async function deleteCredential(id: number): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/credentials/${id}`)
  return data
}

// 获取指定凭据统计
export async function getCredentialStats(id: number): Promise<CredentialStatsResponse> {
  const { data } = await api.get<CredentialStatsResponse>(`/credentials/${id}/stats`)
  return data
}

// 清空指定凭据统计
export async function resetCredentialStats(id: number): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/stats/reset`)
  return data
}

// 清空全部统计
export async function resetAllStats(): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>('/stats/reset')
  return data
}

// 批量导入 token.json
export async function importTokenJson(
  req: ImportTokenJsonRequest
): Promise<ImportTokenJsonResponse> {
  const { data } = await api.post<ImportTokenJsonResponse>(
    '/credentials/import-token-json',
    req
  )
  return data
}

// 获取全局代理配置
export async function getProxyConfig(): Promise<ProxyConfigResponse> {
  const { data } = await api.get<ProxyConfigResponse>('/proxy')
  return data
}

// 更新全局代理配置
export async function updateProxyConfig(req: UpdateProxyConfigRequest): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>('/proxy', req)
  return data
}

// 获取全局配置
export async function getGlobalConfig(): Promise<GlobalConfigResponse> {
  const { data } = await api.get<GlobalConfigResponse>('/config/global')
  return data
}

// 更新全局配置
export async function updateGlobalConfig(req: UpdateGlobalConfigRequest): Promise<SuccessResponse> {
  const { data } = await api.put<SuccessResponse>('/config/global', req)
  return data
}
