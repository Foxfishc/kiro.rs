import { useEffect, useRef, useState } from 'react'
import { toast } from 'sonner'
import { Loader2, Settings2, Zap } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useSetIdp, useSetCredentialProxy } from '@/hooks/use-credentials'
import { openOverageEnableStream, getOverageStatus } from '@/api/credentials'
import type {
  CredentialStatusItem,
  OverageEvent,
  OverageStatusResponse,
} from '@/types/api'

interface CredentialAdvancedDialogProps {
  credential: CredentialStatusItem
  open: boolean
  onOpenChange: (open: boolean) => void
}

/**
 * 凭据高级设置（Web Portal 相关）
 *
 * 三块：
 *  - Web Portal Idp（默认推断为 Google）
 *  - 凭据级代理（覆盖全局代理；支持特殊值 "direct" 显式直连）
 *  - 开启超额：调用 SSE 流，把过程事件实时显示给用户
 */
export function CredentialAdvancedDialog({
  credential,
  open,
  onOpenChange,
}: CredentialAdvancedDialogProps) {
  // —— Idp ——
  const [idpValue, setIdpValue] = useState(credential.idp ?? '')
  const setIdp = useSetIdp()

  // —— 代理 ——
  const [proxyUrl, setProxyUrl] = useState(credential.proxyUrl ?? '')
  const [proxyUser, setProxyUser] = useState(credential.proxyUsername ?? '')
  const [proxyPass, setProxyPass] = useState('')
  const [clearProxyPass, setClearProxyPass] = useState(false)
  const setProxy = useSetCredentialProxy()

  // —— 开启超额 ——
  const [running, setRunning] = useState(false)
  const [events, setEvents] = useState<OverageEvent[]>([])
  const [overageStatus, setOverageStatus] = useState<OverageStatusResponse | null>(null)
  const streamRef = useRef<{ close: () => void } | null>(null)

  // 弹窗打开时把表单同步成最新值，并主动拉一次 overage 实时状态
  // （list 接口的快照可能滞后；如果上一次任务在后台跑完了，这里能立刻看到结果与失败原因）
  useEffect(() => {
    if (open) {
      setIdpValue(credential.idp ?? '')
      setProxyUrl(credential.proxyUrl ?? '')
      setProxyUser(credential.proxyUsername ?? '')
      setProxyPass('')
      setClearProxyPass(false)
      setEvents([])
      setRunning(false)
      setOverageStatus(null)
      // 异步拉取最新状态；忽略失败（401 / 网络异常等只是拿不到额外信息，不影响其余功能）
      getOverageStatus(credential.id)
        .then((s) => setOverageStatus(s))
        .catch(() => {})
    }
  }, [open, credential])

  // 弹窗关闭时关掉 SSE 流（后台任务仍会跑完）
  useEffect(() => {
    if (!open && streamRef.current) {
      streamRef.current.close()
      streamRef.current = null
    }
  }, [open])

  const handleSaveIdp = () => {
    setIdp.mutate(
      { id: credential.id, idp: idpValue.trim() || null },
      {
        onSuccess: (res) => toast.success(res.message),
        onError: (err) => toast.error('保存 idp 失败: ' + (err as Error).message),
      }
    )
  }

  const handleSaveProxy = () => {
    const trimmedUrl = proxyUrl.trim()
    setProxy.mutate(
      {
        id: credential.id,
        req: {
          proxyUrl: trimmedUrl ? trimmedUrl : null,
          proxyUsername: proxyUser.trim() || null,
          proxyPassword: clearProxyPass ? null : proxyPass || null,
        },
      },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setProxyPass('')
          setClearProxyPass(false)
        },
        onError: (err) => toast.error('保存代理失败: ' + (err as Error).message),
      }
    )
  }

  const handleEnableOverage = () => {
    if (running) return
    if (!hasProfileArn) {
      toast.error('凭据缺少 profileArn，无法开启超额（请先刷新 Token）')
      return
    }
    setEvents([])
    setRunning(true)
    streamRef.current = openOverageEnableStream(
      credential.id,
      (event) => {
        setEvents((prev) => [...prev, event])
        if (event.kind === 'done') {
          toast.success('已成功开启超额')
          setRunning(false)
        } else if (event.kind === 'error') {
          toast.error('开启超额失败: ' + event.message)
          setRunning(false)
        }
      },
      (err) => {
        toast.error('SSE 连接异常: ' + String(err))
        setRunning(false)
      }
    )
  }

  // overage 当前状态徽章
  // 优先用主动拉取的 overageStatus（实时），其次回退到 list 快照（可能有延迟）
  const overageEnabling =
    overageStatus?.enabling ?? credential.overageEnabling ?? false
  const overageEnabled =
    overageStatus?.enabled ?? credential.overageEnabled ?? null
  const overageLastError =
    overageStatus?.lastError ?? credential.overageLastError ?? null

  const overageBadge = (() => {
    if (running || overageEnabling) {
      return <Badge variant="secondary">开启中</Badge>
    }
    if (overageEnabled === true) {
      return <Badge>已开启</Badge>
    }
    if (overageEnabled === false) {
      return <Badge variant="outline">未开启</Badge>
    }
    return <Badge variant="outline">未知</Badge>
  })()

  const hasProfileArn =
    overageStatus?.hasProfileArn ?? credential.hasProfileArn ?? false
  const authMethod = overageStatus?.authMethod ?? credential.authMethod ?? null
  const isSocial = !authMethod || authMethod === 'social'

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg max-h-[85vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Settings2 className="h-4 w-4" />
            凭据 #{credential.id} 高级设置
          </DialogTitle>
          <DialogDescription>
            Web Portal Idp、凭据级代理、开启超额（仅 social 凭据支持）
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-6 py-2">
          {/* Idp */}
          <section className="space-y-2">
            <div className="text-sm font-medium">Web Portal Idp</div>
            <p className="text-xs text-muted-foreground">
              默认按认证方式推断（social → Google）。仅当账号实际使用其他 idp
              时才需手动覆盖；空值表示恢复推断。
            </p>
            <div className="flex gap-2">
              <Input
                placeholder="如 Google（留空表示按认证方式推断）"
                value={idpValue}
                onChange={(e) => setIdpValue(e.target.value)}
                disabled={!isSocial}
              />
              <Button
                size="sm"
                onClick={handleSaveIdp}
                disabled={setIdp.isPending || !isSocial}
              >
                保存
              </Button>
            </div>
          </section>

          {/* 代理 */}
          <section className="space-y-2">
            <div className="text-sm font-medium">凭据级代理</div>
            <p className="text-xs text-muted-foreground">
              覆盖全局代理。留空表示回退全局；填 <code>direct</code> 表示这条凭据
              显式直连（即使全局有代理也不走）。
            </p>
            <Input
              placeholder="http://host:port、socks5://host:port、direct 或留空"
              value={proxyUrl}
              onChange={(e) => setProxyUrl(e.target.value)}
            />
            <div className="grid grid-cols-2 gap-2">
              <Input
                placeholder="代理用户名（可选）"
                value={proxyUser}
                onChange={(e) => setProxyUser(e.target.value)}
              />
              <Input
                placeholder={
                  credential.hasProxyPassword
                    ? '已设置（留空保留）'
                    : '代理密码（可选）'
                }
                type="password"
                value={proxyPass}
                onChange={(e) => setProxyPass(e.target.value)}
                disabled={clearProxyPass}
              />
            </div>
            {credential.hasProxyPassword && (
              <label className="flex items-center gap-2 text-xs text-muted-foreground">
                <input
                  type="checkbox"
                  checked={clearProxyPass}
                  onChange={(e) => setClearProxyPass(e.target.checked)}
                />
                清除已保存的代理密码
              </label>
            )}
            <div className="flex justify-end">
              <Button
                size="sm"
                onClick={handleSaveProxy}
                disabled={setProxy.isPending}
              >
                保存代理
              </Button>
            </div>
          </section>

          {/* 开启超额 */}
          <section className="space-y-2">
            <div className="flex items-center justify-between">
              <div className="text-sm font-medium flex items-center gap-2">
                <Zap className="h-4 w-4" />
                超额（Overage）
              </div>
              {overageBadge}
            </div>
            <p className="text-xs text-muted-foreground">
              点击下面按钮会调用 Web Portal 把 overageEnabled 置为 true，
              然后每秒轮询 GetUserUsageAndLimits 直到生效（最多 30 秒）。
              客户端关掉对话框后台任务仍会跑完，下次打开页面可以看到结果。
            </p>
            {overageLastError && (
              <div className="rounded border border-destructive/40 bg-destructive/10 px-2 py-1 text-xs text-destructive">
                上次失败：{overageLastError}
              </div>
            )}
            <div className="flex items-center gap-2">
              <Button
                size="sm"
                onClick={handleEnableOverage}
                disabled={running || !isSocial || !hasProfileArn}
              >
                {running ? (
                  <>
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    进行中
                  </>
                ) : (
                  '开启超额'
                )}
              </Button>
              {!isSocial && (
                <span className="text-xs text-muted-foreground">
                  仅 social 凭据支持 Web Portal
                </span>
              )}
              {isSocial && !hasProfileArn && (
                <span className="text-xs text-muted-foreground">
                  缺少 profileArn，请先刷新 Token
                </span>
              )}
            </div>

            {events.length > 0 && (
              <div className="mt-2 max-h-48 overflow-y-auto rounded border bg-muted/40 p-2 text-xs font-mono space-y-1">
                {events.map((ev, idx) => (
                  <div key={idx}>{formatEvent(ev)}</div>
                ))}
              </div>
            )}
          </section>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            关闭
          </Button>
        </DialogFooter>
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
