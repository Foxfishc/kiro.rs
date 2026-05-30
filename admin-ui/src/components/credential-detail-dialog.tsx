import { useEffect, useState, useRef } from 'react'
import { toast } from 'sonner'
import {
  Copy,
  RefreshCw,
  Download,
  Loader2,
  Zap,
} from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Progress } from '@/components/ui/progress'
import type {
  CredentialStatusItem,
  CachedBalanceInfo,
  BalanceResponse,
  OverageEvent,
  OverageStatusResponse,
} from '@/types/api'
import {
  useForceRefreshToken,
  useSetCredentialProxy,
} from '@/hooks/use-credentials'
import {
  openOverageStream,
  getOverageStatus,
  getCredentialBalance,
} from '@/api/credentials'

interface CredentialDetailDialogProps {
  credential: CredentialStatusItem
  open: boolean
  onOpenChange: (open: boolean) => void
  cachedBalance?: CachedBalanceInfo
  balance: BalanceResponse | null
  loadingBalance: boolean
  onViewBalance: (id: number, forceRefresh: boolean) => void
}

export function CredentialDetailDialog({
  credential,
  open,
  onOpenChange,
  cachedBalance,
  balance,
  loadingBalance,
  onViewBalance,
}: CredentialDetailDialogProps) {
  // 代理编辑
  const [editingProxy, setEditingProxy] = useState(false)
  const [proxyUrl, setProxyUrl] = useState(credential.proxyUrl ?? '')
  const [proxyUser, setProxyUser] = useState(credential.proxyUsername ?? '')
  const [proxyPass, setProxyPass] = useState('')
  const setProxy = useSetCredentialProxy()

  // 刷新 Token
  const forceRefreshToken = useForceRefreshToken()

  // 超额
  const [running, setRunning] = useState(false)
  const [events, setEvents] = useState<OverageEvent[]>([])
  const [overageStatus, setOverageStatus] = useState<OverageStatusResponse | null>(null)
  const streamRef = useRef<{ close: () => void } | null>(null)

  // 本地余额（弹窗内自动拉取）
  const [localBalance, setLocalBalance] = useState<BalanceResponse | null>(null)
  const [localBalanceLoading, setLocalBalanceLoading] = useState(false)

  // 弹窗打开时同步状态
  useEffect(() => {
    if (open) {
      setEditingProxy(false)
      setProxyUrl(credential.proxyUrl ?? '')
      setProxyUser(credential.proxyUsername ?? '')
      setProxyPass('')
      setEvents([])
      setRunning(false)
      setOverageStatus(null)
      setLocalBalance(null)

      // 拉取 overage 状态
      getOverageStatus(credential.id)
        .then((s) => setOverageStatus(s))
        .catch(() => {})

      // 如果没有外部传入的余额，自动拉取
      if (!balance && !loadingBalance) {
        setLocalBalanceLoading(true)
        getCredentialBalance(credential.id)
          .then((b) => setLocalBalance(b))
          .catch(() => {})
          .finally(() => setLocalBalanceLoading(false))
      }
    }
  }, [open, credential])

  // 弹窗关闭时关掉 SSE 流
  useEffect(() => {
    if (!open && streamRef.current) {
      streamRef.current.close()
      streamRef.current = null
    }
  }, [open])

  const effectiveBalance = balance || localBalance
  const isBalanceLoading = loadingBalance || localBalanceLoading

  // 复制到剪贴板
  const copyToClipboard = (text: string, label: string) => {
    navigator.clipboard.writeText(text).then(
      () => toast.success(`${label} 已复制`),
      () => toast.error('复制失败')
    )
  }

  // 刷新 Token
  const handleRefreshToken = () => {
    forceRefreshToken.mutate(credential.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error('刷新失败: ' + (err as Error).message),
    })
  }

  // 导出凭据
  const handleExportCredential = () => {
    const exportData = {
      id: credential.id,
      email: credential.email || credential.accountEmail,
      authMethod: credential.authMethod,
      refreshTokenHash: credential.refreshTokenHash,
      region: credential.region,
      endpoint: credential.endpoint,
    }
    const blob = new Blob([JSON.stringify(exportData, null, 2)], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = `credential-${credential.id}.json`
    a.click()
    URL.revokeObjectURL(url)
    toast.success('凭据已导出')
  }

  // 保存代理
  const handleSaveProxy = () => {
    const trimmedUrl = proxyUrl.trim()
    setProxy.mutate(
      {
        id: credential.id,
        req: {
          proxyUrl: trimmedUrl || null,
          proxyUsername: proxyUser.trim() || null,
          proxyPassword: proxyPass || null,
        },
      },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingProxy(false)
        },
        onError: (err) => toast.error('保存代理失败: ' + (err as Error).message),
      }
    )
  }

  // 开启超额

  const handleEnableOverage = () => {
    setRunning(true)
    setEvents([])
    const handle = openOverageStream(
      credential.id,
      true,
      (ev: OverageEvent) => {
        setEvents((prev) => [...prev, ev])
        if (ev.kind === 'done') {
          setRunning(false)
        } else if (ev.kind === 'error') {
          setRunning(false)
        }
      },
      () => setRunning(false)
    )
    streamRef.current = handle
  }

  // 关闭超额
  const handleDisableOverage = () => {
    setRunning(true)
    setEvents([])
    const handle = openOverageStream(
      credential.id,
      false,
      (ev: OverageEvent) => {
        setEvents((prev) => [...prev, ev])
        if (ev.kind === 'done') {
          setRunning(false)
          toast.success('超额已关闭')
        } else if (ev.kind === 'error') {
          setRunning(false)
          toast.error('关闭超额失败: ' + ev.message)
        }
      },
      () => setRunning(false)
    )
    streamRef.current = handle
  }

  const overageEnabled = overageStatus?.enabled ?? effectiveBalance?.overageEnabled ?? cachedBalance?.overageEnabled ?? credential.overageEnabled ?? null
  const overageCapability = effectiveBalance?.overageCapability ?? cachedBalance?.overageCapability ?? null
  const overageSupported = overageCapability === 'OVERAGE_CAPABLE'
  const effectiveBalanceTotalLimit = effectiveBalance?.usageLimit ?? 0
  const effectiveBalanceOverageCap = effectiveBalance?.overageEnabled ? (effectiveBalance.overageCap ?? 0) : 0
  const effectiveBalanceBaseLimit = effectiveBalance?.overageEnabled
    ? Math.max(0, effectiveBalanceTotalLimit - effectiveBalanceOverageCap)
    : effectiveBalanceTotalLimit
  const effectiveBalanceUsed = effectiveBalance?.currentUsage ?? 0
  const effectiveBalancePercentage = effectiveBalanceTotalLimit > 0
    ? (effectiveBalanceUsed / effectiveBalanceTotalLimit) * 100
    : 0
  const effectiveBalanceRemaining = effectiveBalance?.remaining ?? Math.max(0, effectiveBalanceTotalLimit - effectiveBalanceUsed)
  const effectiveOverageUsed = Math.max(0, effectiveBalanceUsed - effectiveBalanceBaseLimit)
  const effectiveOverageCost = effectiveOverageUsed * 0.04
  const cachedBalanceTotalLimit = cachedBalance?.usageLimit ?? 0
  const cachedBalanceOverageCap = cachedBalance?.overageEnabled ? (cachedBalance.overageCap ?? 0) : 0
  const cachedBalanceBaseLimit = cachedBalance?.overageEnabled
    ? Math.max(0, cachedBalanceTotalLimit - cachedBalanceOverageCap)
    : cachedBalanceTotalLimit
  const cachedBalanceUsed = cachedBalance ? Math.max(0, cachedBalanceTotalLimit - cachedBalance.remaining) : 0
  const cachedBalancePercentage = cachedBalanceTotalLimit > 0
    ? (cachedBalanceUsed / cachedBalanceTotalLimit) * 100
    : 0
  const cachedBalanceRemaining = cachedBalance?.remaining ?? Math.max(0, cachedBalanceTotalLimit - cachedBalanceUsed)
  const cachedOverageUsed = cachedBalance ? Math.max(0, cachedBalanceUsed - cachedBalanceBaseLimit) : 0
  const cachedOverageCost = cachedOverageUsed * 0.04

  // 格式化过期时间
  const formatExpiry = (expiresAt: string | null) => {
    if (!expiresAt) return '未知'
    const date = new Date(expiresAt)
    return date.toLocaleString('zh-CN')
  }

  // 截断显示
  const truncate = (str: string | null | undefined, len: number = 20) => {
    if (!str) return '—'
    if (str.length <= len) return str
    return str.slice(0, len) + '...'
  }

  const formatNumber = (num: number | null | undefined) => {
    return (num ?? 0).toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })
  }

  const formatInteger = (num: number | null | undefined) => {
    return (num ?? 0).toLocaleString('zh-CN')
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            凭据 #{credential.id} 详情
          </DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          {/* 卡片 1: 凭证信息 */}
          <section className="rounded-lg border p-4 space-y-3">
            <h3 className="text-sm font-semibold">凭证信息</h3>
            <div className="space-y-2 text-sm">
              {/* 邮箱 */}
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">邮箱</span>
                <div className="flex items-center gap-1">
                  <span className="font-mono text-xs">
                    {credential.email || credential.accountEmail || '—'}
                  </span>
                  {(credential.email || credential.accountEmail) && (
                    <button
                      onClick={() => copyToClipboard(credential.email || credential.accountEmail || '', '邮箱')}
                      className="p-1 rounded hover:bg-muted"
                    >
                      <Copy className="h-3 w-3 text-muted-foreground" />
                    </button>
                  )}
                </div>
              </div>

              {/* RefreshToken Hash */}
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">RefreshToken</span>
                <div className="flex items-center gap-1">
                  <span className="font-mono text-xs">
                    {truncate(credential.refreshTokenHash, 24)}
                  </span>
                  {credential.refreshTokenHash && (
                    <button
                      onClick={() => copyToClipboard(credential.refreshTokenHash || '', 'RefreshToken Hash')}
                      className="p-1 rounded hover:bg-muted"
                    >
                      <Copy className="h-3 w-3 text-muted-foreground" />
                    </button>
                  )}
                </div>
              </div>

              {/* ProfileARN */}
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">ProfileARN</span>
                <span className="font-mono text-xs">
                  {credential.hasProfileArn ? '✓ 已配置' : '✕ 未配置'}
                </span>
              </div>

              {/* 认证方式 */}
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">认证方式</span>
                <span className="text-xs">{credential.authMethod || '—'}</span>
              </div>

              {/* 过期时间 */}
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">过期时间</span>
                <span className="text-xs">{formatExpiry(credential.expiresAt)}</span>
              </div>

              {/* Endpoint */}
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">Endpoint</span>
                <span className="text-xs font-mono">
                  {credential.endpoint || '默认'} ({credential.effectiveEndpoint})
                </span>
              </div>

              {/* Region */}
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">Region</span>
                <span className="text-xs">
                  {credential.region || '全局默认'}
                  {credential.apiRegion && ` / API: ${credential.apiRegion}`}
                </span>
              </div>
            </div>

            {/* 操作链接 */}
            <div className="flex items-center gap-4 pt-2 border-t">
              <button
                onClick={handleExportCredential}
                className="text-xs text-blue-600 hover:underline flex items-center gap-1"
              >
                <Download className="h-3 w-3" />
                导出凭据
              </button>
              <button
                onClick={handleRefreshToken}
                disabled={forceRefreshToken.isPending}
                className="text-xs text-blue-600 hover:underline flex items-center gap-1 disabled:opacity-50"
              >
                <RefreshCw className={`h-3 w-3 ${forceRefreshToken.isPending ? 'animate-spin' : ''}`} />
                刷新 Token
              </button>
            </div>
          </section>

          {/* 卡片 2: 代理设置 */}
          <section className="rounded-lg border p-4 space-y-2">
            <div className="flex items-center justify-between">
              <h3 className="text-sm font-semibold">代理设置</h3>
              <Button
                size="sm"
                variant="outline"
                className="h-7 text-xs"
                onClick={() => setEditingProxy(!editingProxy)}
              >
                {editingProxy ? '取消' : '编辑'}
              </Button>
            </div>

            {editingProxy ? (
              <div className="space-y-2">
                <Input
                  placeholder="代理 URL（如 socks5://host:port，留空清除）"
                  value={proxyUrl}
                  onChange={(e) => setProxyUrl(e.target.value)}
                  className="h-8 text-sm"
                />
                <div className="flex gap-2">
                  <Input
                    placeholder="用户名（可选）"
                    value={proxyUser}
                    onChange={(e) => setProxyUser(e.target.value)}
                    className="h-8 text-sm flex-1"
                  />
                  <Input
                    placeholder="密码（可选）"
                    type="password"
                    value={proxyPass}
                    onChange={(e) => setProxyPass(e.target.value)}
                    className="h-8 text-sm flex-1"
                  />
                </div>
                <Button
                  size="sm"
                  onClick={handleSaveProxy}
                  disabled={setProxy.isPending}
                  className="h-7 text-xs"
                >
                  保存代理
                </Button>
              </div>
            ) : (
              <p className="text-xs text-muted-foreground">
                {credential.proxyUrl
                  ? `${credential.proxyUrl}${credential.proxyUsername ? ` (用户: ${credential.proxyUsername})` : ''}`
                  : '未配置（使用全局代理）'}
              </p>
            )}
          </section>

          {/* 卡片 3: 用量/额度 */}
          <section className="rounded-lg border p-4 space-y-3">
            <div className="flex items-center justify-between gap-3">
              <h3 className="text-sm font-semibold">用量 / 额度</h3>
              <div className="flex items-center gap-2">
                {overageEnabled ? (
                  <Button
                    size="sm"
                    variant="destructive"
                    className="h-7 text-xs"
                    onClick={handleDisableOverage}
                  >
                    关闭超额
                  </Button>
                ) : overageSupported ? (
                  <Button
                    size="sm"
                    className="h-7 text-xs"
                    onClick={handleEnableOverage}
                    disabled={running}
                  >
                    {running ? (
                      <>
                        <Loader2 className="mr-1 h-3 w-3 animate-spin" />
                        进行中
                      </>
                    ) : (
                      <>
                        <Zap className="mr-1 h-3 w-3" />
                        开启超额
                      </>
                    )}
                  </Button>
                ) : null}
              </div>
            </div>

            {isBalanceLoading ? (
              <div className="flex items-center justify-center py-4">
                <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
              </div>
            ) : effectiveBalance ? (
              <div className="space-y-2">
                <div className="flex justify-between text-sm">
                  <span>已使用: ${formatNumber(effectiveBalanceUsed)}</span>
                  <span>限额: ${formatNumber(effectiveBalanceTotalLimit)}</span>
                </div>
                <Progress
                  value={effectiveBalancePercentage}
                  className="h-2 bg-muted"
                />
                {overageEnabled && (
                  <div className="space-y-1 rounded border bg-purple-50/70 p-2 text-xs dark:bg-purple-950/20">
                    <div className="flex justify-between">
                      <span className="text-muted-foreground">总额度</span>
                      <span className="font-medium">基础 ${formatNumber(effectiveBalanceBaseLimit)} + 超额 ${formatNumber(effectiveBalanceOverageCap)}</span>
                    </div>
                    <div className="flex justify-between">
                      <span className="text-muted-foreground">超额用量</span>
                      <span className="font-medium">${formatNumber(effectiveOverageUsed)} / ${formatNumber(effectiveBalanceOverageCap)}{effectiveOverageUsed > 0 ? `，$${formatNumber(effectiveOverageCost)}` : ''}</span>
                    </div>
                    <div className="flex justify-between">
                      <span className="text-muted-foreground">剩余额度</span>
                      <span className="font-medium text-green-600">${formatNumber(effectiveBalanceRemaining)}</span>
                    </div>
                  </div>
                )}
                <div className="flex items-center justify-between gap-3">
                  <span className="text-xs text-muted-foreground">
                    {effectiveBalancePercentage.toFixed(1)}% 已使用
                  </span>
                  <Button
                    size="sm"
                    variant="outline"
                    className="h-7 text-xs"
                    onClick={() => onViewBalance(credential.id, true)}
                  >
                    刷新余额
                  </Button>
                </div>
              </div>
            ) : cachedBalance && cachedBalance.usageLimit > 0 ? (
              <div className="space-y-2">
                <div className="flex justify-between text-sm">
                  <span>已使用: ${formatNumber(cachedBalanceUsed)}</span>
                  <span>限额: ${formatNumber(cachedBalanceTotalLimit)}</span>
                </div>
                <Progress
                  value={cachedBalancePercentage}
                  className="h-2 bg-muted"
                />
                {overageEnabled && (
                  <div className="space-y-1 rounded border bg-purple-50/70 p-2 text-xs dark:bg-purple-950/20">
                    <div className="flex justify-between">
                      <span className="text-muted-foreground">总额度</span>
                      <span className="font-medium">基础 ${formatNumber(cachedBalanceBaseLimit)} + 超额 ${formatNumber(cachedBalanceOverageCap)}</span>
                    </div>
                    <div className="flex justify-between">
                      <span className="text-muted-foreground">超额用量</span>
                      <span className="font-medium">${formatNumber(cachedOverageUsed)} / ${formatNumber(cachedBalanceOverageCap)}{cachedOverageUsed > 0 ? `，$${formatNumber(cachedOverageCost)}` : ''}</span>
                    </div>
                    <div className="flex justify-between">
                      <span className="text-muted-foreground">剩余额度</span>
                      <span className="font-medium text-green-600">${formatNumber(cachedBalanceRemaining)}</span>
                    </div>
                  </div>
                )}
                <div className="flex items-center justify-between gap-3">
                  <span className="text-xs text-muted-foreground">
                    {cachedBalancePercentage.toFixed(1)}% 已使用（缓存）
                  </span>
                  <Button
                    size="sm"
                    variant="outline"
                    className="h-7 text-xs"
                    onClick={() => onViewBalance(credential.id, true)}
                  >
                    刷新余额
                  </Button>
                </div>
              </div>
            ) : (
              <div className="flex items-center justify-between">
                <span className="text-xs text-muted-foreground">暂无余额数据</span>
                <Button
                  size="sm"
                  variant="outline"
                  className="h-7 text-xs"
                  onClick={() => onViewBalance(credential.id, true)}
                >
                  查询余额
                </Button>
              </div>
            )}

            {!overageSupported && !overageEnabled && (
              <p className="text-xs text-muted-foreground">当前套餐不支持开启超额。</p>
            )}

            {/* 超额事件日志 */}
            {events.length > 0 && (
              <div className="mt-2 max-h-32 overflow-y-auto rounded border bg-muted/40 p-2 text-xs font-mono space-y-0.5">
                {events.map((ev, idx) => (
                  <div key={idx}>{formatEvent(ev)}</div>
                ))}
              </div>
            )}
          </section>


          {/* 统计信息（额外） */}
          <section className="rounded-lg border p-4 space-y-2">
            <h3 className="text-sm font-semibold">调用统计</h3>
            <div className="grid grid-cols-2 gap-2 text-xs">
              <div className="flex justify-between">
                <span className="text-muted-foreground">总调用</span>
                <span className="font-medium">{formatInteger(credential.callsTotal)}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">成功</span>
                <span className="font-medium text-green-600">{formatInteger(credential.callsOk)}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">失败</span>
                <span className="font-medium text-red-500">{formatInteger(credential.callsErr)}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">失败计数</span>
                <span className={credential.failureCount > 0 ? 'font-medium text-red-500' : 'font-medium'}>
                  {formatInteger(credential.failureCount)}
                </span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">输入 Tokens</span>
                <span className="font-medium">{formatInteger(credential.inputTokensTotal)}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">输出 Tokens</span>
                <span className="font-medium">{formatInteger(credential.outputTokensTotal)}</span>
              </div>
            </div>
            {credential.lastError && (
              <div className="mt-2 rounded border border-destructive/40 bg-destructive/10 px-2 py-1 text-xs text-destructive truncate">
                最近错误：{credential.lastError}
              </div>
            )}
          </section>
        </div>
      </DialogContent>
    </Dialog>
  )
}

function formatEvent(ev: OverageEvent): string {
  switch (ev.kind) {
    case 'prepared':
      return `prepared idp=${ev.idp} hasProfileArn=${ev.hasProfileArn}`
    case 'submittingUpdate':
      return 'submitting UpdateBillingPreferences...'
    case 'updateAccepted':
      return 'update accepted, start polling'
    case 'pollingStarted':
      return `polling every ${ev.intervalMs}ms, timeout ${ev.timeoutMs}ms`
    case 'pollTick':
      return `tick #${ev.attempt} elapsed=${ev.elapsedMs}ms overageEnabled=${
        ev.overageEnabled === null ? 'null' : ev.overageEnabled
      }`
    case 'done':
      return `done ✅ overageEnabled=${ev.overageEnabled}`
    case 'error':
      return `error ❌ ${ev.message}`
  }
}
