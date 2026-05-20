import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  getCredentials,
  deleteCredential,
  setCredentialDisabled,
  setCredentialPriority,
  setCredentialRegion,
  setCredentialEndpoint,
  setCredentialIdp,
  setCredentialProxy,
  resetCredentialFailure,
  forceRefreshToken,
  getCredentialBalance,
  getCachedBalances,
  getCredentialAccountInfo,
  addCredential,
  getCredentialStats,
  resetCredentialStats,
  resetAllStats,
  importTokenJson,
  getProxyConfig,
  updateProxyConfig,
  getGlobalConfig,
  updateGlobalConfig,
} from '@/api/credentials'
import type {
  AddCredentialRequest,
  ImportTokenJsonRequest,
  UpdateGlobalConfigRequest,
  SetCredentialProxyRequest,
} from '@/types/api'

// 查询凭据列表
export function useCredentials() {
  return useQuery({
    queryKey: ['credentials'],
    queryFn: getCredentials,
    refetchInterval: 30000, // 每 30 秒刷新一次
  })
}

// 强制刷新 Token
export function useForceRefreshToken() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => forceRefreshToken(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['cached-balances'] })
    },
  })
}

// 查询凭据余额
export function useCredentialBalance(id: number | null) {
  return useQuery({
    queryKey: ['credential-balance', id],
    queryFn: () => getCredentialBalance(id!),
    enabled: id !== null,
    retry: false, // 余额查询失败时不重试（避免重复请求被封禁的账号）
  })
}

// 查询所有凭据的缓存余额（定时轮询，带退避策略）
export function useCachedBalances() {
  return useQuery({
    queryKey: ['cached-balances'],
    queryFn: getCachedBalances,
    refetchInterval: (query) => (query.state.error ? 60000 : 30000),
    refetchIntervalInBackground: false, // 页面不可见时暂停轮询
  })
}

// 查询凭据账号信息（套餐/用量/邮箱等）
export function useCredentialAccountInfo(id: number | null, enabled: boolean) {
  return useQuery({
    queryKey: ['credential-account', id],
    queryFn: () => getCredentialAccountInfo(id!),
    enabled: enabled && id !== null,
    retry: false,
  })
}

// 删除指定凭据
export function useDeleteCredential() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => deleteCredential(id),
    onSuccess: (_res, id) => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['credential-balance', id] })
      queryClient.invalidateQueries({ queryKey: ['credential-account', id] })
      queryClient.invalidateQueries({ queryKey: ['credential-stats', id] })
    },
  })
}

// 设置禁用状态
export function useSetDisabled() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, disabled }: { id: number; disabled: boolean }) =>
      setCredentialDisabled(id, disabled),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 设置优先级
export function useSetPriority() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, priority }: { id: number; priority: number }) =>
      setCredentialPriority(id, priority),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 设置 Region
export function useSetRegion() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, region, apiRegion }: { id: number; region: string | null; apiRegion: string | null }) =>
      setCredentialRegion(id, region, apiRegion),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 设置 endpoint
export function useSetEndpoint() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, endpoint }: { id: number; endpoint: string | null }) =>
      setCredentialEndpoint(id, endpoint),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 设置凭据级 idp
export function useSetIdp() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, idp }: { id: number; idp: string | null }) =>
      setCredentialIdp(id, idp),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 设置凭据级代理
export function useSetCredentialProxy() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, req }: { id: number; req: SetCredentialProxyRequest }) =>
      setCredentialProxy(id, req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 重置失败计数
export function useResetFailure() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => resetCredentialFailure(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 添加新凭据
export function useAddCredential() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: AddCredentialRequest) => addCredential(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 查询指定凭据统计
export function useCredentialStats(id: number | null, enabled: boolean) {
  return useQuery({
    queryKey: ['credential-stats', id],
    queryFn: () => getCredentialStats(id!),
    enabled: enabled && id !== null,
    retry: false,
  })
}

// 清空指定凭据统计
export function useResetCredentialStats() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => resetCredentialStats(id),
    onSuccess: (_res, id) => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['credential-stats', id] })
    },
  })
}

// 清空全部统计
export function useResetAllStats() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: () => resetAllStats(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['credential-stats'] })
    },
  })
}

// 批量导入 token.json
export function useImportTokenJson() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: ImportTokenJsonRequest) => importTokenJson(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['cached-balances'] })
    },
  })
}

// 查询全局代理配置
export function useProxyConfig() {
  return useQuery({
    queryKey: ['proxyConfig'],
    queryFn: getProxyConfig,
  })
}

// 更新全局代理配置
export function useUpdateProxyConfig() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: updateProxyConfig,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxyConfig'] })
      toast.success('全局代理配置已更新')
    },
    onError: (error: any) => {
      toast.error(error.response?.data?.error?.message || '更新失败')
    },
  })
}

// 查询全局配置
export function useGlobalConfig() {
  return useQuery({
    queryKey: ['globalConfig'],
    queryFn: getGlobalConfig,
  })
}

// 更新全局配置
export function useUpdateGlobalConfig() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: UpdateGlobalConfigRequest) => updateGlobalConfig(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['globalConfig'] })
      queryClient.invalidateQueries({ queryKey: ['proxyConfig'] })
      toast.success('全局配置已更新')
    },
    onError: (error: any) => {
      toast.error(error.response?.data?.error?.message || '更新失败')
    },
  })
}
