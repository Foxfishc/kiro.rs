import { useRef, useState } from 'react'
import { toast } from 'sonner'
import {
  RefreshCw,
  ChevronUp,
  ChevronDown,
  Wallet,
  Trash2,
  Loader2,
  Edit3,
  Download,
  CheckCircle2,
  Zap,
} from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Switch } from '@/components/ui/switch'
import { Input } from '@/components/ui/input'
import { Checkbox } from '@/components/ui/checkbox'
import { Progress } from '@/components/ui/progress'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import type { CredentialStatusItem, CachedBalanceInfo, BalanceResponse, OverageEvent } from '@/types/api'
import {
  useSetDisabled,
  useSetPriority,
  useResetFailure,
  useForceRefreshToken,
  useDeleteCredential,
  useSetRegion,
  useSetEndpoint,
} from '@/hooks/use-credentials'
import { getCredentialBalance, openOverageStream } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import { CredentialDetailDialog } from '@/components/credential-detail-dialog'

interface CredentialCardProps {
  credential: CredentialStatusItem
  cachedBalance?: CachedBalanceInfo
  onViewBalance: (id: number, forceRefresh: boolean) => void
  selected: boolean
  onToggleSelect: () => void
  balance: BalanceResponse | null
  loadingBalance: boolean
}

interface SingleVerifyReport {
  status: 'verifying' | 'success' | 'failed'
  startedAt: number
  completedAt?: number
  balance?: BalanceResponse
  error?: string
}

function formatLastUsed(lastUsedAt: string | null): string {
  if (!lastUsedAt) return '从未使用'
  const date = new Date(lastUsedAt)
  const now = new Date()
  const diff = now.getTime() - date.getTime()
  if (diff < 0) return '刚刚'
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `${seconds} 秒前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} 分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时前`
  const days = Math.floor(hours / 24)
  return `${days} 天前`
}

function formatNumber(num: number | null | undefined, digits = 2): string {
  return (num ?? 0).toLocaleString('zh-CN', {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  })
}

function formatInteger(num: number | null | undefined): string {
  return (num ?? 0).toLocaleString('zh-CN')
}

function getProxyDisplay(credential: CredentialStatusItem): string {
  if (credential.proxyUrl) {
    if (credential.proxyUrl.toLowerCase() === 'direct') return '直连（不使用代理）'
    return `${credential.proxyUrl}${credential.proxyUsername ? ` (用户: ${credential.proxyUsername})` : ''}`
  }
  return credential.hasProxy ? '已配置代理' : '未配置（使用全局代理）'
}

export function CredentialCard({
  credential,
  cachedBalance,
  onViewBalance,
  selected,
  onToggleSelect,
  balance,
  loadingBalance,
}: CredentialCardProps) {
  const [editingPriority, setEditingPriority] = useState(false)
  const [priorityValue, setPriorityValue] = useState(String(credential.priority))
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)
  const [showDetailDialog, setShowDetailDialog] = useState(false)
  const [showVerifyDialog, setShowVerifyDialog] = useState(false)
  const [verifyReport, setVerifyReport] = useState<SingleVerifyReport | null>(null)
  const [editingEndpointRegion, setEditingEndpointRegion] = useState(false)
  const [endpointValue, setEndpointValue] = useState(credential.endpoint ?? '')
  const [regionValue, setRegionValue] = useState(credential.region ?? '')
  const [apiRegionValue, setApiRegionValue] = useState(credential.apiRegion ?? '')
  const [overageRunning, setOverageRunning] = useState(false)
  const streamRef = useRef<{ close: () => void } | null>(null)

  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const resetFailure = useResetFailure()
  const forceRefreshToken = useForceRefreshToken()
  const deleteCredential = useDeleteCredential()
  const setRegion = useSetRegion()
  const setEndpoint = useSetEndpoint()
  const queriedOverageEnabled = balance?.overageEnabled ?? cachedBalance?.overageEnabled ?? credential.overageEnabled ?? null
  const overageEnabled = queriedOverageEnabled === true || credential.overageEnabling === true
  const overageCap = overageEnabled ? (balance?.overageCap ?? cachedBalance?.overageCap ?? 10000) : 0
  const overageCapability = balance?.overageCapability ?? cachedBalance?.overageCapability ?? null
  const overageSupported = overageCapability === 'OVERAGE_CAPABLE'

  const handleToggleDisabled = () => {
    setDisabled.mutate(
      { id: credential.id, disabled: !credential.disabled },
      {
        onSuccess: (res) => {
          toast.success(res.message)
        },
        onError: (err) => {
          toast.error('操作失败: ' + (err as Error).message)
        },
      }
    )
  }

  const handlePriorityChange = () => {
    const newPriority = parseInt(priorityValue, 10)
    if (isNaN(newPriority) || newPriority < 0) {
      toast.error('优先级必须是非负整数')
      return
    }
    setPriority.mutate(
      { id: credential.id, priority: newPriority },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingPriority(false)
        },
        onError: (err) => {
          toast.error('操作失败: ' + (err as Error).message)
        },
      }
    )
  }

  const handleReset = () => {
    resetFailure.mutate(credential.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error('操作失败: ' + (err as Error).message),
    })
  }

  const handleForceRefresh = () => {
    forceRefreshToken.mutate(credential.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error('操作失败: ' + (err as Error).message),
    })
  }

  const handleDelete = () => {
    deleteCredential.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
        setShowDeleteDialog(false)
      },
      onError: (err) => toast.error('删除失败: ' + (err as Error).message),
    })
  }

  const handleViewBalance = () => {
    onViewBalance(credential.id, true)
  }

  const handleSingleVerify = async () => {
    const startedAt = Date.now()
    setVerifyReport({ status: 'verifying', startedAt })
    setShowVerifyDialog(true)
    toast.info(`正在单独测活凭据 #${credential.id}`)

    try {
      const result = await getCredentialBalance(credential.id)
      setVerifyReport({
        status: 'success',
        startedAt,
        completedAt: Date.now(),
        balance: result,
      })
      toast.success(`凭据 #${credential.id} 测活成功`)
    } catch (error) {
      setVerifyReport({
        status: 'failed',
        startedAt,
        completedAt: Date.now(),
        error: extractErrorMessage(error),
      })
      toast.error(`凭据 #${credential.id} 测活失败`)
    }
  }

  const handleExportCredential = () => {
    const data = {
      id: credential.id,
      authMethod: credential.authMethod,
      email: credential.email || credential.accountEmail,
      subscriptionTitle: credential.subscriptionTitle,
      endpoint: credential.endpoint,
      effectiveEndpoint: credential.effectiveEndpoint,
      region: credential.region,
      apiRegion: credential.apiRegion,
      proxyUrl: credential.proxyUrl,
      proxyUsername: credential.proxyUsername,
      hasProfileArn: credential.hasProfileArn,
      overageEnabled: credential.overageEnabled,
    }
    const content = JSON.stringify(data, null, 2)
    try {
      const blob = new Blob([content], { type: 'application/json' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = `credential-${credential.id}.json`
      document.body.appendChild(a)
      a.click()
      a.remove()
      URL.revokeObjectURL(url)
      toast.success('凭据摘要 JSON 已导出')
    } catch {
      navigator.clipboard?.writeText(content).then(
        () => toast.success('下载失败，已复制 JSON 到剪贴板'),
        () => toast.error('导出失败，请检查浏览器下载权限')
      )
    }
  }

  const handleSaveEndpointRegion = () => {
    const endpoint = endpointValue.trim() || null
    const region = regionValue.trim() || null
    const apiRegion = apiRegionValue.trim() || null

    setEndpoint.mutate(
      { id: credential.id, endpoint },
      {
        onSuccess: () => {
          setRegion.mutate(
            { id: credential.id, region, apiRegion },
            {
              onSuccess: () => {
                toast.success(`凭据 #${credential.id} Endpoint / Region 已更新`)
                setEditingEndpointRegion(false)
              },
              onError: (err) => toast.error('Region 更新失败: ' + (err as Error).message),
            }
          )
        },
        onError: (err) => toast.error('Endpoint 更新失败: ' + (err as Error).message),
      }
    )
  }

  const handleCancelEndpointRegionEdit = () => {
    setEndpointValue(credential.endpoint ?? '')
    setRegionValue(credential.region ?? '')
    setApiRegionValue(credential.apiRegion ?? '')
    setEditingEndpointRegion(false)
  }

  const handleToggleOverage = () => {
    const targetEnabled = !overageEnabled
    setOverageRunning(true)
    const handle = openOverageStream(
      credential.id,
      targetEnabled,
      (ev: OverageEvent) => {
        if (ev.kind === 'done') {
          setOverageRunning(false)
          toast.success(`凭据 #${credential.id} 已${targetEnabled ? '开启' : '关闭'} Overages`)
        } else if (ev.kind === 'error') {
          setOverageRunning(false)
          toast.error(`${targetEnabled ? '开启' : '关闭'} Overages 失败: ` + ev.message)
        }
      },
      (err) => {
        setOverageRunning(false)
        toast.error(`${targetEnabled ? '开启' : '关闭'} Overages 失败: ` + extractErrorMessage(err))
      }
    )
    streamRef.current = handle
  }

  // 格式化缓存时间（相对时间）
  const formatCacheAge = (cachedAt: number) => {
    const now = Date.now()
    const diff = now - cachedAt
    const seconds = Math.floor(diff / 1000)
    if (seconds < 60) return `${seconds}秒前`
    const minutes = Math.floor(seconds / 60)
    if (minutes < 60) return `${minutes}分钟前`
    return `${Math.floor(minutes / 60)}小时前`
  }

  // 获取余额显示数据
  const getBalanceDisplay = () => {
    if (loadingBalance) return { loading: true }
    if (balance) {
      const totalLimit = balance.usageLimit
      const cap = balance.overageEnabled ? (balance.overageCap ?? 0) : 0
      const baseLimit = balance.overageEnabled ? Math.max(0, totalLimit - cap) : totalLimit
      return {
        used: balance.currentUsage,
        baseLimit,
        limit: totalLimit,
        percentage: balance.usagePercentage,
      }
    }
    if (cachedBalance && cachedBalance.usageLimit > 0) {
      const totalLimit = cachedBalance.usageLimit
      const cap = cachedBalance.overageEnabled ? (cachedBalance.overageCap ?? 0) : 0
      const baseLimit = cachedBalance.overageEnabled ? Math.max(0, totalLimit - cap) : totalLimit
      const used = totalLimit - cachedBalance.remaining
      return {
        used,
        baseLimit,
        limit: totalLimit,
        percentage: cachedBalance.usagePercentage,
        cached: true,
        cacheAge: formatCacheAge(cachedBalance.cachedAt),
      }
    }
    return null
  }

  const balanceDisplay = getBalanceDisplay()
  const used = balanceDisplay && !('loading' in balanceDisplay) ? balanceDisplay.used ?? 0 : null
  const limit = balanceDisplay && !('loading' in balanceDisplay) ? balanceDisplay.limit ?? 0 : null
  const baseLimit = balanceDisplay && !('loading' in balanceDisplay) ? (balanceDisplay as { baseLimit?: number }).baseLimit ?? limit ?? 0 : null
  const percentage = balanceDisplay && !('loading' in balanceDisplay) ? balanceDisplay.percentage ?? 0 : null
  const overageUsed = used !== null && baseLimit !== null ? Math.max(0, used - baseLimit) : null
  const overageCost = overageUsed !== null ? overageUsed * 0.04 : null
  const proxyDisplay = getProxyDisplay(credential)
  const verifyDurationSecs = verifyReport?.completedAt
    ? ((verifyReport.completedAt - verifyReport.startedAt) / 1000).toFixed(2)
    : null
  const verifyBalance = verifyReport?.balance
  const verifyTotalLimit = verifyBalance?.usageLimit ?? 0
  const verifyCap = verifyBalance?.overageEnabled ? (verifyBalance.overageCap ?? 0) : 0
  const verifyBaseLimit = verifyBalance?.overageEnabled ? Math.max(0, verifyTotalLimit - verifyCap) : verifyTotalLimit
  const verifyUsagePercentage = verifyBalance?.usagePercentage ?? 0
  const verifyRemaining = verifyBalance?.remaining ?? (verifyTotalLimit - (verifyBalance?.currentUsage ?? 0))
  const verifyOverageUsed = Math.max(0, (verifyBalance?.currentUsage ?? 0) - verifyBaseLimit)
  const verifyOverageCost = verifyOverageUsed * 0.04

  return (
    <>
      <Card
        className="cursor-pointer border-border bg-card text-card-foreground shadow-sm transition-all duration-200 hover:-translate-y-0.5 hover:border-primary/30 hover:shadow-lg hover:shadow-primary/5"
        onClick={(e) => {
          const target = e.target as HTMLElement
          if (
            target.closest('button') ||
            target.closest('[role="checkbox"]') ||
            target.closest('[role="switch"]') ||
            target.closest('input') ||
            target.closest('select')
          ) {
            return
          }
          setShowDetailDialog(true)
        }}
      >
        <CardHeader className="pb-3">
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0 flex items-start gap-2">
              <Checkbox
                checked={selected}
                onCheckedChange={onToggleSelect}
                className="mt-0.5"
              />
              <div className="min-w-0 space-y-1">
                <CardTitle className="flex min-w-0 items-center gap-2 text-base">
                  <span className="truncate">凭据 #{credential.id}</span>
                  {credential.disabled && (
                    <Badge variant="destructive" className="text-[10px]">已禁用</Badge>
                  )}
                </CardTitle>
                <div className="flex flex-wrap gap-1.5">
                  <Badge variant="secondary" className="text-[10px] capitalize">
                    {credential.authMethod || 'unknown'}
                  </Badge>
                  <Badge variant="outline" className="text-[10px]">
                    {credential.effectiveEndpoint || credential.endpoint || 'ide'}
                  </Badge>
                </div>
              </div>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <span className="text-xs text-muted-foreground">启用</span>
              <Switch
                checked={!credential.disabled}
                onCheckedChange={handleToggleDisabled}
                disabled={setDisabled.isPending}
              />
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-3 text-xs">
          <div className="space-y-1.5">
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">优先级：</span>
              {editingPriority ? (
                <div className="inline-flex items-center gap-1">
                  <Input
                    type="number"
                    value={priorityValue}
                    onChange={(e) => setPriorityValue(e.target.value)}
                    className="h-6 w-16 text-xs"
                    min="0"
                    onClick={(e) => e.stopPropagation()}
                  />
                  <Button size="sm" variant="ghost" className="h-6 w-6 p-0 text-xs" onClick={(e) => { e.stopPropagation(); handlePriorityChange() }} disabled={setPriority.isPending}>✓</Button>
                  <Button size="sm" variant="ghost" className="h-6 w-6 p-0 text-xs" onClick={(e) => { e.stopPropagation(); setEditingPriority(false); setPriorityValue(String(credential.priority)) }}>✕</Button>
                </div>
              ) : (
                <button
                  className="font-medium hover:underline"
                  onClick={(e) => { e.stopPropagation(); setEditingPriority(true) }}
                >
                  {credential.priority}（点击编辑）
                </button>
              )}
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">失败次数：</span>
              <span className={credential.failureCount > 0 ? 'font-medium text-red-500' : 'font-medium'}>{formatInteger(credential.failureCount)}</span>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">刷新失败：</span>
              <span className={credential.refreshFailureCount > 0 ? 'font-medium text-red-500' : 'font-medium'}>{formatInteger(credential.refreshFailureCount)}</span>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">订阅等级：</span>
              <span className="font-medium truncate">
                {loadingBalance ? <Loader2 className="inline h-3 w-3 animate-spin" /> : balance?.subscriptionTitle ?? cachedBalance?.subscriptionTitle ?? credential.subscriptionTitle ?? '未知'}
              </span>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">成功次数：</span>
              <span className="font-medium">{formatInteger(credential.successCount ?? credential.callsOk)}</span>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">最后调用：</span>
              <span className="font-medium">{formatLastUsed(credential.lastUsedAt || credential.lastCallAt)}</span>
            </div>
          </div>

          {balanceDisplay && 'loading' in balanceDisplay && balanceDisplay.loading && (
            <div className="flex items-center gap-1 rounded-md bg-muted px-2 py-2 text-muted-foreground">
              <Loader2 className="h-3 w-3 animate-spin" /> 加载余额...
            </div>
          )}

          {balanceDisplay && !('loading' in balanceDisplay && balanceDisplay.loading) && 'percentage' in balanceDisplay && (
            <div className="space-y-1.5 rounded-md border border-border bg-muted/50 p-2 text-foreground">
              <div className="flex items-center justify-between gap-3">
                <span className="text-muted-foreground">已使用：</span>
                <span className="font-medium">
                  {formatNumber(used, 2)} / {formatNumber(limit, 2)} ({formatNumber(percentage, 1)}% 已用)
                </span>
              </div>
              <Progress value={percentage ?? 0} className="h-2 bg-muted" />
              {'cached' in balanceDisplay && balanceDisplay.cached && (
                <div className="text-[11px] text-muted-foreground">{balanceDisplay.cacheAge}缓存</div>
              )}
              {overageEnabled && (
                <div className="space-y-1 text-[11px]">
                  <div className="flex items-center justify-between gap-3">
                    <span className="font-medium text-purple-600 dark:text-purple-300">Overages：Enabled</span>
                    <span className="text-muted-foreground">总额度 = 基础 {formatNumber(baseLimit, 0)} + 超额 {formatNumber(overageCap, 0)}</span>
                  </div>
                  <div className="flex items-center justify-between gap-3">
                    <span className="text-muted-foreground">超额用量：</span>
                    {overageUsed !== null && overageUsed > 0 ? (
                      <span className="text-muted-foreground">
                        {formatNumber(overageUsed, 2)} / {formatNumber(overageCap, 2)}，${formatNumber(overageCost, 2)}
                      </span>
                    ) : (
                      <span className="text-muted-foreground">0 / {formatNumber(overageCap, 0)}</span>
                    )}
                  </div>
                </div>
              )}
            </div>
          )}

          {!balanceDisplay && overageEnabled && (
            <div className="rounded-md border border-border bg-muted/50 p-2 text-foreground">
              <span className="font-medium text-purple-600 dark:text-purple-300">Overages：Enabled</span>
              <span className="ml-2 text-muted-foreground">可额外使用 {formatNumber(overageCap, 0)}</span>
            </div>
          )}

          <div className="space-y-1.5">
            {editingEndpointRegion ? (
              <div className="space-y-2 rounded-md border border-border bg-muted/50 p-2 text-foreground" onClick={(e) => e.stopPropagation()}>
                <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
                  <Input className="h-7 text-xs" placeholder="Endpoint（留空默认）" value={endpointValue} onChange={(e) => setEndpointValue(e.target.value)} />
                  <Input className="h-7 text-xs" placeholder="Region（留空默认）" value={regionValue} onChange={(e) => setRegionValue(e.target.value)} />
                  <Input className="h-7 text-xs" placeholder="API Region（可选）" value={apiRegionValue} onChange={(e) => setApiRegionValue(e.target.value)} />
                </div>
                <div className="flex justify-end gap-2">
                  <Button size="sm" variant="ghost" className="h-7 text-xs" onClick={handleCancelEndpointRegionEdit}>取消</Button>
                  <Button size="sm" className="h-7 text-xs" onClick={handleSaveEndpointRegion} disabled={setEndpoint.isPending || setRegion.isPending}>保存 Endpoint / Region</Button>
                </div>
              </div>
            ) : (
              <div className="flex items-center justify-between gap-2">
                <div className="min-w-0 text-muted-foreground">Endpoint：<span className="font-mono text-foreground">{credential.endpoint || '默认'} ({credential.effectiveEndpoint})</span></div>
                <Button size="sm" variant="ghost" className="h-6 px-2 text-xs" onClick={(e) => { e.stopPropagation(); setEditingEndpointRegion(true) }}>编辑</Button>
              </div>
            )}
            <div className="flex items-start gap-2">
              <span className="shrink-0 text-muted-foreground">代理：</span>
              <span className="min-w-0 truncate font-mono text-[11px]" title={proxyDisplay}>{proxyDisplay}</span>
            </div>
            <div className="flex flex-wrap gap-1.5">
              {credential.hasProfileArn && <Badge variant="secondary" className="text-[10px]">有 Profile ARN</Badge>}
              {credential.hasProxy && <Badge variant="secondary" className="text-[10px]">凭据代理</Badge>}
              {credential.endpoint && <Badge variant="outline" className="text-[10px]">Endpoint: {credential.endpoint}</Badge>}
              {credential.region && <Badge variant="outline" className="text-[10px]">{credential.region}{credential.apiRegion ? ` / ${credential.apiRegion}` : ''}</Badge>}
            </div>
          </div>

          <div className="grid grid-cols-3 gap-1.5 border-t pt-3">
            <Button size="sm" variant="outline" className="h-8 px-2 text-xs" onClick={(e) => { e.stopPropagation(); handleReset() }} disabled={resetFailure.isPending || credential.failureCount === 0}>
              <RefreshCw className="mr-1 h-3 w-3" />重置失败
            </Button>
            <Button size="sm" variant="outline" className="h-8 px-2 text-xs" onClick={(e) => { e.stopPropagation(); handleForceRefresh() }} disabled={forceRefreshToken.isPending}>
              <RefreshCw className={`mr-1 h-3 w-3 ${forceRefreshToken.isPending ? 'animate-spin' : ''}`} />刷新 Token
            </Button>
            <Button size="sm" variant="outline" className="h-8 px-2 text-xs" onClick={(e) => { e.stopPropagation(); const newPriority = Math.max(0, credential.priority - 1); setPriority.mutate({ id: credential.id, priority: newPriority }, { onSuccess: (res) => toast.success(res.message), onError: (err) => toast.error('操作失败: ' + (err as Error).message) }) }} disabled={setPriority.isPending || credential.priority === 0}>
              <ChevronUp className="mr-1 h-3 w-3" />提高优先级
            </Button>
            <Button size="sm" variant="outline" className="h-8 px-2 text-xs" onClick={(e) => { e.stopPropagation(); const newPriority = credential.priority + 1; setPriority.mutate({ id: credential.id, priority: newPriority }, { onSuccess: (res) => toast.success(res.message), onError: (err) => toast.error('操作失败: ' + (err as Error).message) }) }} disabled={setPriority.isPending}>
              <ChevronDown className="mr-1 h-3 w-3" />降低优先级
            </Button>
            <Button size="sm" variant="outline" className="h-8 px-2 text-xs" onClick={(e) => { e.stopPropagation(); setShowDetailDialog(true) }}>
              <Edit3 className="mr-1 h-3 w-3" />编辑
            </Button>
            <Button size="sm" variant="outline" className="h-8 px-2 text-xs" onClick={(e) => { e.stopPropagation(); handleExportCredential() }}>
              <Download className="mr-1 h-3 w-3" />导出 JSON
            </Button>
            <Button size="sm" variant="outline" className="h-8 px-2 text-xs" onClick={(e) => { e.stopPropagation(); handleSingleVerify() }} disabled={verifyReport?.status === 'verifying'}>
              <CheckCircle2 className={`mr-1 h-3 w-3 ${verifyReport?.status === 'verifying' ? 'animate-spin' : ''}`} />单独测活
            </Button>
            {overageSupported || overageEnabled ? (
              <Button size="sm" variant={overageEnabled ? 'outline' : 'secondary'} className="h-8 px-2 text-xs" onClick={(e) => { e.stopPropagation(); handleToggleOverage() }} disabled={overageRunning}>
                {overageRunning ? <Loader2 className="mr-1 h-3 w-3 animate-spin" /> : <Zap className="mr-1 h-3 w-3" />}
                {overageEnabled ? '关闭 Overages' : overageRunning ? '开启中' : '开启 Overages'}
              </Button>
            ) : null}
            <Button size="sm" variant="default" className="h-8 px-2 text-xs" onClick={(e) => { e.stopPropagation(); handleViewBalance() }}>
              <Wallet className="mr-1 h-3 w-3" />查看余额
            </Button>
          </div>

          <Button
            size="sm"
            variant="destructive"
            className="h-8 w-full bg-red-100 text-red-700 hover:bg-red-200 dark:bg-red-950 dark:text-red-200"
            onClick={(e) => { e.stopPropagation(); setShowDeleteDialog(true) }}
            disabled={!credential.disabled}
            title={!credential.disabled ? '需要先禁用凭据才能删除' : undefined}
          >
            <Trash2 className="mr-1 h-3 w-3" />删除
          </Button>
        </CardContent>
      </Card>

      {/* 删除确认对话框 */}
      <Dialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>确认删除凭据</DialogTitle>
            <DialogDescription>
              您确定要删除凭据 #{credential.id} 吗？此操作无法撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowDeleteDialog(false)}
              disabled={deleteCredential.isPending}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleteCredential.isPending || !credential.disabled}
            >
              确认删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={showVerifyDialog} onOpenChange={setShowVerifyDialog}>
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              {verifyReport?.status === 'verifying' ? (
                <Loader2 className="h-5 w-5 animate-spin text-blue-500" />
              ) : verifyReport?.status === 'success' ? (
                <CheckCircle2 className="h-5 w-5 text-green-600" />
              ) : (
                <span className="text-red-600">✗</span>
              )}
              凭据 #{credential.id} 测活报告
            </DialogTitle>
            <DialogDescription>
              单独请求该凭据余额接口，用于验证 token、Profile ARN、代理和上游连通性。
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4 py-2 text-sm">
            <div
              className={
                verifyReport?.status === 'success'
                  ? 'rounded-md border border-green-200 bg-green-50 p-3 text-green-800 dark:border-green-900 dark:bg-green-950 dark:text-green-200'
                  : verifyReport?.status === 'failed'
                    ? 'rounded-md border border-red-200 bg-red-50 p-3 text-red-800 dark:border-red-900 dark:bg-red-950 dark:text-red-200'
                    : 'rounded-md border border-blue-200 bg-blue-50 p-3 text-blue-800 dark:border-blue-900 dark:bg-blue-950 dark:text-blue-200'
              }
            >
              {verifyReport?.status === 'verifying' && '正在测活，请稍候...'}
              {verifyReport?.status === 'success' && '测活成功：凭据可用，余额接口返回正常。'}
              {verifyReport?.status === 'failed' && '测活失败：该凭据当前不可用或上游/代理请求失败。'}
            </div>

            <div className="grid gap-2 rounded-md border p-3 text-xs">
              <div className="flex justify-between gap-3">
                <span className="text-muted-foreground">凭据 ID：</span>
                <span className="font-medium">#{credential.id}</span>
              </div>
              <div className="flex justify-between gap-3">
                <span className="text-muted-foreground">认证方式：</span>
                <span className="font-medium capitalize">{credential.authMethod || 'unknown'}</span>
              </div>
              <div className="flex justify-between gap-3">
                <span className="text-muted-foreground">Endpoint：</span>
                <span className="font-medium">{credential.effectiveEndpoint || credential.endpoint || 'ide'}</span>
              </div>
              <div className="flex items-start justify-between gap-3">
                <span className="shrink-0 text-muted-foreground">代理：</span>
                <span className="min-w-0 break-all text-right font-mono">{proxyDisplay}</span>
              </div>
              <div className="flex justify-between gap-3">
                <span className="text-muted-foreground">Profile ARN：</span>
                <span className="font-medium">{credential.hasProfileArn ? '有' : '无'}</span>
              </div>
              {verifyDurationSecs && (
                <div className="flex justify-between gap-3">
                  <span className="text-muted-foreground">耗时：</span>
                  <span className="font-medium">{verifyDurationSecs} 秒</span>
                </div>
              )}
            </div>

            {verifyBalance && (
              <div className="space-y-2 rounded-md border border-border bg-muted/50 p-3 text-foreground">
                <div className="flex justify-between gap-3 text-xs">
                  <span className="text-muted-foreground">订阅等级：</span>
                  <span className="font-medium">{verifyBalance.subscriptionTitle || '未知'}</span>
                </div>
                <div className="flex justify-between gap-3 text-xs">
                  <span className="text-muted-foreground">已使用：</span>
                  <span className="font-medium">
                    {formatNumber(verifyBalance.currentUsage)} / {formatNumber(verifyTotalLimit)} ({formatNumber(verifyUsagePercentage, 1)}% 已用)
                  </span>
                </div>
                <Progress value={verifyUsagePercentage} className="h-2" />
                {verifyBalance.overageEnabled && (
                  <div className="space-y-1 rounded border border-purple-200 bg-purple-50/70 p-2 text-xs dark:border-purple-800 dark:bg-purple-950/40">
                    <div className="flex justify-between gap-3">
                      <span className="text-muted-foreground">Overages：</span>
                      <span className="font-medium text-purple-600 dark:text-purple-300">Enabled</span>
                    </div>
                    <div className="flex justify-between gap-3">
                      <span className="text-muted-foreground">总额度：</span>
                      <span className="font-medium">基础 {formatNumber(verifyBaseLimit, 0)} + 超额 {formatNumber(verifyCap, 0)}</span>
                    </div>
                    <div className="flex justify-between gap-3">
                      <span className="text-muted-foreground">超额用量：</span>
                      <span className="font-medium">{formatNumber(verifyOverageUsed)} / {formatNumber(verifyCap)}{verifyOverageUsed > 0 ? `，$${formatNumber(verifyOverageCost)}` : ''}</span>
                    </div>
                  </div>
                )}
                <div className="flex justify-between gap-3 text-xs">
                  <span className="text-muted-foreground">剩余额度：</span>
                  <span className="font-medium text-green-600">{formatNumber(verifyRemaining)}</span>
                </div>
              </div>
            )}

            {verifyReport?.error && (
              <div className="rounded-md border border-red-200 bg-red-50 p-3 text-xs text-red-700 dark:border-red-900 dark:bg-red-950 dark:text-red-200">
                <div className="mb-1 font-medium">错误信息</div>
                <div className="break-words">{verifyReport.error}</div>
              </div>
            )}
          </div>

          <DialogFooter>
            <Button variant="outline" onClick={() => handleSingleVerify()} disabled={verifyReport?.status === 'verifying'}>
              {verifyReport?.status === 'verifying' && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              重新测活
            </Button>
            <Button onClick={() => setShowVerifyDialog(false)} disabled={verifyReport?.status === 'verifying'}>
              关闭
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>


      <CredentialDetailDialog
        credential={credential}
        open={showDetailDialog}
        onOpenChange={setShowDetailDialog}
        cachedBalance={cachedBalance}
        balance={balance}
        loadingBalance={loadingBalance}
        onViewBalance={onViewBalance}
      />
    </>
  )
}

