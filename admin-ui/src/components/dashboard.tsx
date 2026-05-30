import { useState, useEffect, useRef, useMemo } from 'react'
import { RefreshCw, LogOut, Moon, Sun, Server, Plus, Upload, Trash2, RotateCcw, CheckCircle2, Globe, ArrowUp, ArrowDown, Boxes } from 'lucide-react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { storage } from '@/lib/storage'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { CredentialCard } from '@/components/credential-card'
import { BalanceDialog } from '@/components/balance-dialog'
import { AddCredentialDialog } from '@/components/add-credential-dialog'
import { ImportTokenJsonDialog } from '@/components/import-token-json-dialog'
import { BatchVerifyDialog, type VerifyResult } from '@/components/batch-verify-dialog'
import { ProxyConfigDialog } from '@/components/proxy-config-dialog'
import { GlobalConfigDialog } from '@/components/global-config-dialog'
import { AvailableModelsDialog } from '@/components/available-models-dialog'
import { useCredentials, useCachedBalances, useDeleteCredential, useResetFailure, useForceRefreshToken, useProxyConfig, useGlobalConfig } from '@/hooks/use-credentials'
import { getCredentialBalance } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import { Badge } from '@/components/ui/badge'
import type { BalanceResponse } from '@/types/api'

type SortField = 'default' | 'id' | 'balance'
type SortOrder = 'asc' | 'desc'

interface DashboardProps {
  onLogout: () => void
}

export function Dashboard({ onLogout }: DashboardProps) {
  const [selectedCredentialId, setSelectedCredentialId] = useState<number | null>(null)
  const [balanceDialogOpen, setBalanceDialogOpen] = useState(false)
  const [forceRefreshBalance, setForceRefreshBalance] = useState(false)
  const [addDialogOpen, setAddDialogOpen] = useState(false)
  const [importDialogOpen, setImportDialogOpen] = useState(false)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [verifyDialogOpen, setVerifyDialogOpen] = useState(false)
  const [proxyConfigDialogOpen, setProxyConfigDialogOpen] = useState(false)
  const [globalConfigDialogOpen, setGlobalConfigDialogOpen] = useState(false)
  const [availableModelsDialogOpen, setAvailableModelsDialogOpen] = useState(false)
  const [verifying, setVerifying] = useState(false)
  const [verifyProgress, setVerifyProgress] = useState({ current: 0, total: 0 })
  const [verifyResults, setVerifyResults] = useState<Map<number, VerifyResult>>(new Map())
  const [balanceMap, setBalanceMap] = useState<Map<number, BalanceResponse>>(new Map())
  const [loadingBalanceIds, setLoadingBalanceIds] = useState<Set<number>>(new Set())
  const [queryingInfo, setQueryingInfo] = useState(false)
  const [queryInfoProgress, setQueryInfoProgress] = useState({ current: 0, total: 0 })
  const cancelVerifyRef = useRef(false)
  const [currentPage, setCurrentPage] = useState(1)
  const [sortField, setSortField] = useState<SortField>('default')
  const [sortOrder, setSortOrder] = useState<SortOrder>('asc')
  const itemsPerPage = 12
  const [darkMode, setDarkMode] = useState(() => {
    if (typeof window !== 'undefined') {
      return document.documentElement.classList.contains('dark')
    }
    return false
  })

  // 首屏同步：确保 React 状态与 index.html 提前应用的 .dark class 一致，
  // 并把当前主题持久化（兼容旧版本未存储 adminTheme 的情况）。
  useEffect(() => {
    const isDark = storage.getTheme() === 'dark'
    document.documentElement.classList.toggle('dark', isDark)
    setDarkMode(isDark)
  }, [])

  const queryClient = useQueryClient()
  const { data, isLoading, error, refetch } = useCredentials()
  const { data: cachedBalancesData } = useCachedBalances()
  const { mutate: deleteCredential } = useDeleteCredential()
  const { mutate: resetFailure } = useResetFailure()
  const { mutate: forceRefreshToken } = useForceRefreshToken()
  const { data: proxyConfig } = useProxyConfig()
  const { data: globalConfig } = useGlobalConfig()

  // 派生数据必须在所有早退 return 之前计算，避免登录后 hook 顺序变化。
  // 但对尚未加载完成的 query 数据要给安全默认值，否则某个非关键
  // query 短暂返回 undefined 时会让 Dashboard 渲染阶段抛错，React 19
  // 在当前生产构建下表现为 root 被清空（白屏）。
  const credentials = data?.credentials ?? []

  // 构建 id -> cachedBalance 的映射
  const cachedBalanceMap = useMemo(
    () => new Map(
      (cachedBalancesData?.balances ?? []).map((b) => [b.id, b])
    ),
    [cachedBalancesData?.balances]
  )

  // 排序后的凭据列表
  const sortedCredentials = useMemo(() => {
    if (sortField === 'default') return credentials

    return [...credentials].sort((a, b) => {
      let cmp = 0
      if (sortField === 'id') {
        cmp = a.id - b.id
      } else if (sortField === 'balance') {
        const balA = cachedBalanceMap.get(a.id)?.remaining ?? -Infinity
        const balB = cachedBalanceMap.get(b.id)?.remaining ?? -Infinity
        cmp = balA - balB
      }
      return sortOrder === 'asc' ? cmp : -cmp
    })
  }, [credentials, sortField, sortOrder, cachedBalanceMap])

  // 计算分页
  const totalPages = Math.ceil(sortedCredentials.length / itemsPerPage)
  const startIndex = (currentPage - 1) * itemsPerPage
  const endIndex = startIndex + itemsPerPage
  const currentCredentials = sortedCredentials.slice(startIndex, endIndex)
  const disabledCredentialCount = data?.credentials.filter(credential => credential.disabled).length || 0
  const selectedDisabledCount = Array.from(selectedIds).filter(id => {
    const credential = data?.credentials.find(c => c.id === id)
    return Boolean(credential?.disabled)
  }).length

  // 当凭据列表变化时重置到第一页
  useEffect(() => {
    setCurrentPage(1)
  }, [data?.credentials.length])

  // 只保留当前仍存在的凭据缓存，避免删除后残留旧数据
  useEffect(() => {
    if (!data?.credentials) {
      setBalanceMap(new Map())
      setLoadingBalanceIds(new Set())
      return
    }

    const validIds = new Set(data.credentials.map(credential => credential.id))

    setBalanceMap(prev => {
      const next = new Map<number, BalanceResponse>()
      prev.forEach((value, id) => {
        if (validIds.has(id)) {
          next.set(id, value)
        }
      })
      return next.size === prev.size ? prev : next
    })

    setLoadingBalanceIds(prev => {
      if (prev.size === 0) {
        return prev
      }
      const next = new Set<number>()
      prev.forEach(id => {
        if (validIds.has(id)) {
          next.add(id)
        }
      })
      return next.size === prev.size ? prev : next
    })
  }, [data?.credentials])

  const toggleDarkMode = () => {
    const next = !darkMode
    setDarkMode(next)
    document.documentElement.classList.toggle('dark', next)
    storage.setTheme(next ? 'dark' : 'light')
  }

  const handleViewBalance = (id: number, forceRefresh: boolean) => {
    setSelectedCredentialId(id)
    setForceRefreshBalance(forceRefresh)
    if (forceRefresh) {
      // 清除该凭据的余额缓存，强制重新获取
      queryClient.invalidateQueries({ queryKey: ['credential-balance', id] })
    }
    setBalanceDialogOpen(true)
  }

  const handleRefresh = () => {
    refetch()
    toast.success('已刷新凭据列表')
  }

  const handleLogout = () => {
    storage.removeApiKey()
    queryClient.clear()
    onLogout()
  }

  // 排序切换
  const handleSortChange = (field: SortField) => {
    if (field === sortField) {
      // 同一字段：切换方向
      setSortOrder(prev => prev === 'asc' ? 'desc' : 'asc')
    } else {
      setSortField(field)
      setSortOrder(field === 'balance' ? 'desc' : 'asc')
    }
    setCurrentPage(1)
  }

  // 选择管理
  const toggleSelect = (id: number) => {
    const newSelected = new Set(selectedIds)
    if (newSelected.has(id)) {
      newSelected.delete(id)
    } else {
      newSelected.add(id)
    }
    setSelectedIds(newSelected)
  }

  const deselectAll = () => {
    setSelectedIds(new Set())
  }

  // 批量删除（仅删除已禁用项）
  const handleBatchDelete = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要删除的凭据')
      return
    }

    const disabledIds = Array.from(selectedIds).filter(id => {
      const credential = data?.credentials.find(c => c.id === id)
      return Boolean(credential?.disabled)
    })

    if (disabledIds.length === 0) {
      toast.error('选中的凭据中没有已禁用项')
      return
    }

    const skippedCount = selectedIds.size - disabledIds.length
    const skippedText = skippedCount > 0 ? `（将跳过 ${skippedCount} 个未禁用凭据）` : ''

    if (!confirm(`确定要删除 ${disabledIds.length} 个已禁用凭据吗？此操作无法撤销。${skippedText}`)) {
      return
    }

    let successCount = 0
    let failCount = 0

    for (const id of disabledIds) {
      try {
        await new Promise<void>((resolve, reject) => {
          deleteCredential(id, {
            onSuccess: () => {
              successCount++
              resolve()
            },
            onError: (err) => {
              failCount++
              reject(err)
            }
          })
        })
      } catch (error) {
        // 错误已在 onError 中处理
      }
    }

    const skippedResultText = skippedCount > 0 ? `，已跳过 ${skippedCount} 个未禁用凭据` : ''

    if (failCount === 0) {
      toast.success(`成功删除 ${successCount} 个已禁用凭据${skippedResultText}`)
    } else {
      toast.warning(`删除已禁用凭据：成功 ${successCount} 个，失败 ${failCount} 个${skippedResultText}`)
    }

    deselectAll()
  }

  // 批量恢复异常
  const handleBatchResetFailure = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要恢复的凭据')
      return
    }

    const failedIds = Array.from(selectedIds).filter(id => {
      const cred = data?.credentials.find(c => c.id === id)
      return cred && (cred.failureCount > 0 || cred.refreshFailureCount > 0)
    })

    if (failedIds.length === 0) {
      toast.error('选中的凭据中没有失败的凭据')
      return
    }

    let successCount = 0
    let failCount = 0

    for (const id of failedIds) {
      try {
        await new Promise<void>((resolve, reject) => {
          resetFailure(id, {
            onSuccess: () => {
              successCount++
              resolve()
            },
            onError: (err) => {
              failCount++
              reject(err)
            }
          })
        })
      } catch (error) {
        // 错误已在 onError 中处理
      }
    }

    if (failCount === 0) {
      toast.success(`成功恢复 ${successCount} 个凭据`)
    } else {
      toast.warning(`成功 ${successCount} 个，失败 ${failCount} 个`)
    }

    deselectAll()
  }

  const handleBatchForceRefresh = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要刷新的凭据')
      return
    }

    const ids = Array.from(selectedIds)
    let successCount = 0
    let failCount = 0

    for (const id of ids) {
      try {
        await new Promise<void>((resolve, reject) => {
          forceRefreshToken(id, {
            onSuccess: () => {
              successCount++
              resolve()
            },
            onError: () => {
              failCount++
              reject(new Error('refresh failed'))
            }
          })
        })
      } catch {
        // noop
      }
    }

    if (failCount === 0) {
      toast.success(`成功刷新 ${successCount} 个凭据`)
    } else {
      toast.warning(`刷新完成：成功 ${successCount} 个，失败 ${failCount} 个`)
    }
  }

  // 一键清除所有已禁用凭据
  const handleClearAll = async () => {
    if (!data?.credentials || data.credentials.length === 0) {
      toast.error('没有可清除的凭据')
      return
    }

    const disabledCredentials = data.credentials.filter(credential => credential.disabled)

    if (disabledCredentials.length === 0) {
      toast.error('没有可清除的已禁用凭据')
      return
    }

    if (!confirm(`确定要清除所有 ${disabledCredentials.length} 个已禁用凭据吗？此操作无法撤销。`)) {
      return
    }

    let successCount = 0
    let failCount = 0

    for (const credential of disabledCredentials) {
      try {
        await new Promise<void>((resolve, reject) => {
          deleteCredential(credential.id, {
            onSuccess: () => {
              successCount++
              resolve()
            },
            onError: (err) => {
              failCount++
              reject(err)
            }
          })
        })
      } catch (error) {
        // 错误已在 onError 中处理
      }
    }

    if (failCount === 0) {
      toast.success(`成功清除所有 ${successCount} 个已禁用凭据`)
    } else {
      toast.warning(`清除已禁用凭据：成功 ${successCount} 个，失败 ${failCount} 个`)
    }

    deselectAll()
  }

  // 查询当前页凭据信息（逐个查询，避免瞬时并发）
  const handleQueryCurrentPageInfo = async () => {
    if (currentCredentials.length === 0) {
      toast.error('当前页没有可查询的凭据')
      return
    }

    const ids = currentCredentials
      .filter(credential => !credential.disabled)
      .map(credential => credential.id)

    if (ids.length === 0) {
      toast.error('当前页没有可查询的启用凭据')
      return
    }

    setQueryingInfo(true)
    setQueryInfoProgress({ current: 0, total: ids.length })

    let successCount = 0
    let failCount = 0

    for (let i = 0; i < ids.length; i++) {
      const id = ids[i]

      setLoadingBalanceIds(prev => {
        const next = new Set(prev)
        next.add(id)
        return next
      })

      try {
        const balance = await getCredentialBalance(id)
        successCount++

        setBalanceMap(prev => {
          const next = new Map(prev)
          next.set(id, balance)
          return next
        })
        queryClient.setQueryData(['credential-balance', id], balance)
        queryClient.invalidateQueries({ queryKey: ['credentials'] })
        queryClient.invalidateQueries({ queryKey: ['cached-balances'] })
      } catch (error) {
        failCount++
      } finally {
        setLoadingBalanceIds(prev => {
          const next = new Set(prev)
          next.delete(id)
          return next
        })
      }

      setQueryInfoProgress({ current: i + 1, total: ids.length })
    }

    setQueryingInfo(false)

    if (failCount === 0) {
      toast.success(`查询完成：成功 ${successCount}/${ids.length}`)
    } else {
      toast.warning(`查询完成：成功 ${successCount} 个，失败 ${failCount} 个`)
    }
  }

  // 批量验活
  const handleBatchVerify = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要验活的凭据')
      return
    }

    // 初始化状态
    setVerifying(true)
    cancelVerifyRef.current = false
    const ids = Array.from(selectedIds)
    setVerifyProgress({ current: 0, total: ids.length })

    let successCount = 0

    // 初始化结果，所有凭据状态为 pending
    const initialResults = new Map<number, VerifyResult>()
    ids.forEach(id => {
      initialResults.set(id, { id, status: 'pending' })
    })
    setVerifyResults(initialResults)
    setVerifyDialogOpen(true)

    // 开始验活
    for (let i = 0; i < ids.length; i++) {
      // 检查是否取消
      if (cancelVerifyRef.current) {
        toast.info('已取消验活')
        break
      }

      const id = ids[i]

      // 更新当前凭据状态为 verifying
      setVerifyResults(prev => {
        const newResults = new Map(prev)
        newResults.set(id, { id, status: 'verifying' })
        return newResults
      })

      try {
        const balance = await getCredentialBalance(id)
        successCount++
        queryClient.setQueryData(['credential-balance', id], balance)
        queryClient.invalidateQueries({ queryKey: ['credentials'] })
        queryClient.invalidateQueries({ queryKey: ['cached-balances'] })

        // 更新为成功状态
        setVerifyResults(prev => {
          const newResults = new Map(prev)
          newResults.set(id, {
            id,
            status: 'success',
            usage: `${balance.currentUsage}/${balance.usageLimit}`
          })
          return newResults
        })
      } catch (error) {
        // 更新为失败状态
        setVerifyResults(prev => {
          const newResults = new Map(prev)
          newResults.set(id, {
            id,
            status: 'failed',
            error: extractErrorMessage(error)
          })
          return newResults
        })
      }

      // 更新进度
      setVerifyProgress({ current: i + 1, total: ids.length })

      // 添加延迟防止封号（最后一个不需要延迟）
      if (i < ids.length - 1 && !cancelVerifyRef.current) {
        await new Promise(resolve => setTimeout(resolve, 2000))
      }
    }

    setVerifying(false)

    if (!cancelVerifyRef.current) {
      toast.success(`验活完成：成功 ${successCount}/${ids.length}`)
    }
  }

  // 取消验活
  const handleCancelVerify = () => {
    cancelVerifyRef.current = true
    setVerifying(false)
  }

  if (isLoading) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background">
        <div className="text-center">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-primary mx-auto mb-4"></div>
          <p className="text-muted-foreground">加载中...</p>
        </div>
      </div>
    )
  }

  if (error) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background p-4">
        <Card className="w-full max-w-md">
          <CardContent className="pt-6 text-center">
            <div className="text-red-500 mb-4">加载失败</div>
            <p className="text-muted-foreground mb-4">{(error as Error).message}</p>
            <div className="space-x-2">
              <Button onClick={() => refetch()}>重试</Button>
              <Button variant="outline" onClick={handleLogout}>重新登录</Button>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="min-h-screen bg-background">
      {/* 顶部导航 */}
      <header className="sticky top-0 z-50 w-full border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/60">
        <div className="container flex h-14 items-center justify-between px-4 md:px-8">
          <div className="flex items-center gap-2">
            <Server className="h-5 w-5" />
            <span className="font-semibold">Kiro Admin</span>
          </div>
          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={() => setAvailableModelsDialogOpen(true)}>
              <Boxes className="h-4 w-4 mr-2" />
              可用模型
            </Button>
            <Button variant="ghost" size="icon" onClick={toggleDarkMode}>
              {darkMode ? <Sun className="h-5 w-5" /> : <Moon className="h-5 w-5" />}
            </Button>
            <Button variant="ghost" size="icon" onClick={handleRefresh}>
              <RefreshCw className="h-5 w-5" />
            </Button>
            <Button variant="ghost" size="icon" onClick={handleLogout}>
              <LogOut className="h-5 w-5" />
            </Button>
          </div>
        </div>
      </header>

      {/* 主内容 */}
      <main className="container mx-auto px-4 md:px-8 py-6">
        {/* 统计卡片 */}
        <div className="grid gap-4 md:grid-cols-3 mb-6">
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">
                凭据总数
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">{data?.total || 0}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">
                可用凭据
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-green-600">{data?.available || 0}</div>
            </CardContent>
          </Card>
          <Card
            className="cursor-pointer hover:border-primary/50 transition-colors"
            onClick={() => setGlobalConfigDialogOpen(true)}
          >
            <CardHeader className="pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground flex items-center gap-1.5">
                <Globe className="h-4 w-4" />
                全局配置
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-xs space-y-0.5">
                <div className="truncate">{globalConfig?.region || '-'} | RPM: {globalConfig?.credentialRpm ?? '默认'}</div>
                <div className="truncate">Endpoint: {globalConfig?.defaultEndpoint || 'ide'} | 压缩: {globalConfig?.compression?.enabled ? '开' : '关'}</div>
                <div className="truncate">代理: {proxyConfig?.proxyUrl || '无'}</div>
              </div>
            </CardContent>
          </Card>
        </div>

        {/* 凭据列表 */}
        <div className="space-y-4">
          <div className="flex items-center justify-between flex-wrap gap-2">
            <div className="flex items-center gap-4">
              <h2 className="text-xl font-semibold">凭据管理</h2>
              {/* 排序控件 */}
              <div className="flex items-center gap-1">
                {([
                  ['default', '默认'],
                  ['id', 'ID'],
                  ['balance', '余额'],
                ] as const).map(([field, label]) => {
                  const active = sortField === field
                  return (
                    <Button
                      key={field}
                      size="sm"
                      variant={active ? 'secondary' : 'ghost'}
                      className="h-7 px-2 text-xs"
                      onClick={() => handleSortChange(field)}
                    >
                      {label}
                      {active && field !== 'default' && (
                        sortOrder === 'asc'
                          ? <ArrowUp className="h-3 w-3 ml-0.5" />
                          : <ArrowDown className="h-3 w-3 ml-0.5" />
                      )}
                    </Button>
                  )
                })}
              </div>
              {selectedIds.size > 0 && (
                <div className="flex items-center gap-2">
                  <Badge variant="secondary">已选择 {selectedIds.size} 个</Badge>
                  <Button onClick={deselectAll} size="sm" variant="ghost">
                    取消选择
                  </Button>
                </div>
              )}
            </div>
            <div className="flex gap-2">
              {selectedIds.size > 0 && (
                <>
                  <Button onClick={handleBatchVerify} size="sm" variant="outline">
                    <CheckCircle2 className="h-4 w-4 mr-2" />
                    批量验活
                  </Button>
                  <Button onClick={handleBatchForceRefresh} size="sm" variant="outline">
                    <RefreshCw className="h-4 w-4 mr-2" />
                    批量刷新
                  </Button>
                  <Button onClick={handleBatchResetFailure} size="sm" variant="outline">
                    <RotateCcw className="h-4 w-4 mr-2" />
                    恢复异常
                  </Button>
                  <Button
                    onClick={handleBatchDelete}
                    size="sm"
                    variant="destructive"
                    disabled={selectedDisabledCount === 0}
                    title={selectedDisabledCount === 0 ? '只能删除已禁用凭据' : undefined}
                  >
                    <Trash2 className="h-4 w-4 mr-2" />
                    批量删除
                  </Button>
                </>
              )}
              {verifying && !verifyDialogOpen && (
                <Button onClick={() => setVerifyDialogOpen(true)} size="sm" variant="secondary">
                  <CheckCircle2 className="h-4 w-4 mr-2 animate-spin" />
                  验活中... {verifyProgress.current}/{verifyProgress.total}
                </Button>
              )}
              {data?.credentials && data.credentials.length > 0 && (
                <Button
                  onClick={handleQueryCurrentPageInfo}
                  size="sm"
                  variant="outline"
                  disabled={queryingInfo}
                >
                  <RefreshCw className={`h-4 w-4 mr-2 ${queryingInfo ? 'animate-spin' : ''}`} />
                  {queryingInfo ? `查询中... ${queryInfoProgress.current}/${queryInfoProgress.total}` : '查询信息'}
                </Button>
              )}
              {data?.credentials && data.credentials.length > 0 && (
                <Button
                  onClick={handleClearAll}
                  size="sm"
                  variant="outline"
                  className="text-destructive hover:text-destructive"
                  disabled={disabledCredentialCount === 0}
                  title={disabledCredentialCount === 0 ? '没有可清除的已禁用凭据' : undefined}
                >
                  <Trash2 className="h-4 w-4 mr-2" />
                  清除已禁用
                </Button>
              )}
              <Button variant="outline" onClick={() => setImportDialogOpen(true)} size="sm">
                <Upload className="h-4 w-4 mr-2" />
                导入凭据
              </Button>
              <Button onClick={() => setAddDialogOpen(true)} size="sm">
                <Plus className="h-4 w-4 mr-2" />
                添加凭据
              </Button>
            </div>
          </div>
          {data?.credentials.length === 0 ? (
            <Card>
              <CardContent className="py-8 text-center text-muted-foreground">
                暂无凭据
              </CardContent>
            </Card>
          ) : (
            <>
              <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
                {currentCredentials.map((credential) => (
                  <CredentialCard
                    key={credential.id}
                    credential={credential}
                    cachedBalance={cachedBalanceMap.get(credential.id)}
                    onViewBalance={handleViewBalance}
                    selected={selectedIds.has(credential.id)}
                    onToggleSelect={() => toggleSelect(credential.id)}
                    balance={balanceMap.get(credential.id) || null}
                    loadingBalance={loadingBalanceIds.has(credential.id)}
                  />
                ))}
              </div>

              {/* 分页控件 */}
              {totalPages > 1 && (
                <div className="flex justify-center items-center gap-4 mt-6">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setCurrentPage(p => Math.max(1, p - 1))}
                    disabled={currentPage === 1}
                  >
                    上一页
                  </Button>
                  <span className="text-sm text-muted-foreground">
                    第 {currentPage} / {totalPages} 页（共 {data?.credentials.length} 个凭据）
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setCurrentPage(p => Math.min(totalPages, p + 1))}
                    disabled={currentPage === totalPages}
                  >
                    下一页
                  </Button>
                </div>
              )}
            </>
          )}
        </div>
      </main>

      {/* 余额对话框 */}
      <BalanceDialog
        credentialId={selectedCredentialId}
        open={balanceDialogOpen}
        onOpenChange={(open) => {
          setBalanceDialogOpen(open)
          if (!open) {
            setForceRefreshBalance(false)
            // 关闭弹窗时刷新缓存余额，让卡片显示最新数据
            queryClient.invalidateQueries({ queryKey: ['cached-balances'] })
          }
        }}
        forceRefresh={forceRefreshBalance}
      />

      {/* 添加凭据对话框 */}
      <AddCredentialDialog
        open={addDialogOpen}
        onOpenChange={setAddDialogOpen}
      />

      {/* 导入凭据对话框 */}
      <ImportTokenJsonDialog
        open={importDialogOpen}
        onOpenChange={setImportDialogOpen}
      />

      {/* 全局配置对话框 */}
      <GlobalConfigDialog
        open={globalConfigDialogOpen}
        onOpenChange={setGlobalConfigDialogOpen}
      />

      {/* 全局可用模型对话框 */}
      <AvailableModelsDialog
        open={availableModelsDialogOpen}
        onOpenChange={setAvailableModelsDialogOpen}
      />

      {/* 全局代理配置对话框（保留兼容） */}
      <ProxyConfigDialog
        open={proxyConfigDialogOpen}
        onOpenChange={setProxyConfigDialogOpen}
      />


      {/* 批量验活对话框 */}
      <BatchVerifyDialog
        open={verifyDialogOpen}
        onOpenChange={setVerifyDialogOpen}
        verifying={verifying}
        progress={verifyProgress}
        results={verifyResults}
        onCancel={handleCancelVerify}
      />
    </div>
  )
}
