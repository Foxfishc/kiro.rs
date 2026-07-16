import { useState, useCallback, useRef } from 'react'
import { toast } from 'sonner'
import { Upload, FileJson, CheckCircle2, XCircle, AlertCircle, Loader2 } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogDescription,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { useImportTokenJson, useDeleteCredential } from '@/hooks/use-credentials'
import { getCredentialBalance, setCredentialDisabled } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { TokenJsonItem, ImportItemResult, ImportSummary } from '@/types/api'

interface ImportTokenJsonDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

type Step = 'input' | 'preview' | 'result' | 'verifying'

// 验活结果
interface VerifyItemResult {
  index: number
  credentialId?: number
  status: 'pending' | 'verifying' | 'verified' | 'failed' | 'skipped' | 'rolled_back' | 'rollback_failed'
  usage?: string
  error?: string
  rollbackError?: string
}

export function ImportTokenJsonDialog({ open, onOpenChange }: ImportTokenJsonDialogProps) {
  const [step, setStep] = useState<Step>('input')
  const [jsonText, setJsonText] = useState('')
  const [parsedItems, setParsedItems] = useState<TokenJsonItem[]>([])
  const [previewResults, setPreviewResults] = useState<ImportItemResult[]>([])
  const [previewSummary, setPreviewSummary] = useState<ImportSummary | null>(null)
  const [finalResults, setFinalResults] = useState<ImportItemResult[]>([])
  const [finalSummary, setFinalSummary] = useState<ImportSummary | null>(null)
  const [isDragging, setIsDragging] = useState(false)
  const [enableVerify, setEnableVerify] = useState(false)
  const [verifyResults, setVerifyResults] = useState<VerifyItemResult[]>([])
  const [verifyProgress, setVerifyProgress] = useState({ current: 0, total: 0 })
  const [isVerifying, setIsVerifying] = useState(false)
  const fileInputRef = useRef<HTMLInputElement>(null)

  const { mutate: importMutate, isPending } = useImportTokenJson()
  const { mutateAsync: deleteCredential } = useDeleteCredential()

  const resetState = useCallback(() => {
    setStep('input')
    setJsonText('')
    setParsedItems([])
    setPreviewResults([])
    setPreviewSummary(null)
    setFinalResults([])
    setFinalSummary(null)
    setEnableVerify(false)
    setVerifyResults([])
    setVerifyProgress({ current: 0, total: 0 })
    setIsVerifying(false)
  }, [])

  const handleClose = useCallback(() => {
    if (isVerifying) return // 验活中不允许关闭
    onOpenChange(false)
    setTimeout(resetState, 200)
  }, [onOpenChange, resetState, isVerifying])

  // 兼容 KAM 1.8.3 新版平铺格式，统一转换为旧格式（credentials 嵌套结构）
  const normalizeKamAccount = useCallback((item: unknown): unknown => {
    if (!item || typeof item !== 'object') return item
    const obj = item as Record<string, unknown>

    // Kiro 登录 harvest bundle 格式（企业 IdC 与 Azure AD external_idp 通用）：
    //   { token: {accessToken,refreshToken,authMethod,region,authRegion,profileArn,apiRegion,
    //             clientId,tokenEndpoint,issuerUrl,scopes,provider,...},
    //     profile: {arn,name}, clientRegistration?: {clientId,clientSecret},
    //     meta: {accountName,startUrl,region,...} }
    // refreshToken 在 token.* 里；clientId/Secret 可能在 clientRegistration.*（IdC）
    // 或直接在 token.*（external_idp，public client 无 secret）。展平成顶层扁平结构。
    if (obj.token && typeof obj.token === 'object') {
      const tok = obj.token as Record<string, unknown>
      const reg = (obj.clientRegistration && typeof obj.clientRegistration === 'object'
        ? obj.clientRegistration : {}) as Record<string, unknown>
      const meta = (obj.meta && typeof obj.meta === 'object' ? obj.meta : {}) as Record<string, unknown>
      const prof = (obj.profile && typeof obj.profile === 'object' ? obj.profile : {}) as Record<string, unknown>
      const rt = typeof tok.refreshToken === 'string' ? tok.refreshToken.trim() : ''
      if (rt) {
        const str = (v: unknown) => (typeof v === 'string' && v.trim() ? v.trim() : undefined)
        return {
          refreshToken: rt,
          accessToken: str(tok.accessToken),
          // clientId：clientRegistration 优先（IdC），回退 token.clientId（external_idp）
          clientId: str(reg.clientId) ?? str(tok.clientId),
          clientSecret: str(reg.clientSecret) ?? str(tok.clientSecret),
          authMethod: str(tok.authMethod) ?? str(meta.authMethod) ?? 'idc',
          // 刷新用 authRegion（token 签发区），API 请求用 apiRegion（profile 所在区）
          region: str(tok.authRegion) ?? str(tok.region) ?? str(meta.region),
          apiRegion: str(tok.apiRegion),
          profileArn: str(tok.profileArn) ?? str(prof.arn),
          provider: str(tok.provider),
          // external_idp（Azure AD）刷新必需的三个字段
          tokenEndpoint: str(tok.tokenEndpoint) ?? str(meta.tokenEndpoint),
          issuerUrl: str(tok.issuerUrl) ?? str(meta.issuerUrl),
          scopes: str(tok.scopes) ?? str(meta.scopes),
          email: str(tok.email) ?? str(meta.accountName) ?? str(obj.email),
          machineId: str(obj.machineId) ?? str(tok.machineId),
        }
      }
    }

    // 新格式：refreshToken 直接在账号对象上，无 credentials 嵌套
    if (typeof obj.refreshToken === 'string' && typeof obj.credentials === 'undefined') {
      const nickname = typeof obj.nickname === 'string'
        ? obj.nickname
        : typeof obj.label === 'string'
          ? obj.label
          : undefined

      return {
        email: typeof obj.email === 'string' ? obj.email : undefined,
        userId:
          typeof obj.userId === 'string' || obj.userId === null
            ? obj.userId
            : undefined,
        nickname,
        status: typeof obj.status === 'string' ? obj.status : undefined,
        machineId: typeof obj.machineId === 'string' ? obj.machineId : undefined,
        credentials: {
          refreshToken: obj.refreshToken,
          clientId: typeof obj.clientId === 'string' ? obj.clientId : undefined,
          clientSecret: typeof obj.clientSecret === 'string' ? obj.clientSecret : undefined,
          region: typeof obj.region === 'string' ? obj.region : undefined,
          authMethod: typeof obj.authMethod === 'string' ? obj.authMethod : undefined,
          startUrl: typeof obj.startUrl === 'string' ? obj.startUrl : undefined,
        },
      }
    }

    return item
  }, [])

  // 将 KAM 账号结构展平为 TokenJsonItem
  const flattenKamAccount = useCallback((account: Record<string, unknown>): TokenJsonItem | null => {
    const cred = account.credentials as Record<string, unknown> | undefined
    if (!cred || typeof cred !== 'object') return null
    // refreshToken 必须是非空字符串
    if (typeof cred.refreshToken !== 'string' || !cred.refreshToken.trim()) return null
    // 跳过 error 状态的账号
    if (account.status === 'error') return null
    const authMethod = cred.authMethod as string | undefined
    return {
      refreshToken: cred.refreshToken.trim(),
      clientId: cred.clientId as string | undefined,
      clientSecret: cred.clientSecret as string | undefined,
      authMethod: (!authMethod && cred.clientId && cred.clientSecret) ? 'idc' : authMethod,
      region: cred.region as string | undefined,
      machineId: account.machineId as string | undefined,
    }
  }, [])

  // 解析核心：返回 { items, error }，不弹 toast。供 parseJson（会弹 toast）
  // 与 parseJsonSilent（多文件汇总时静默）共用。
  const parseJsonCore = useCallback((text: string): { items: TokenJsonItem[] | null; error: string | null } => {
    let parsed: unknown
    try {
      parsed = JSON.parse(text)
    } catch {
      // 非 JSON：按「每行一个 API key」的纯文本批量导入处理（对齐 Kiro-Go apikeys-batch）。
      const keys = text
        .split(/\r?\n/)
        .map(l => l.trim())
        .filter(l => l.length > 0 && !l.startsWith('#'))
      if (keys.length > 0) {
        const items = keys.map(k => ({ kiroApiKey: k, authMethod: 'api_key' }) as TokenJsonItem)
        return { items, error: null }
      }
      return { items: null, error: 'JSON 格式无效' }
    }

    let rawItems: unknown[]
    const p = parsed as Record<string, unknown>
    // KAM 标准导出格式：{ version, accounts: [...] }
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed) && Array.isArray(p.accounts)) {
      rawItems = p.accounts as unknown[]
    } else if (Array.isArray(parsed)) {
      rawItems = parsed
    } else if (parsed && typeof parsed === 'object') {
      rawItems = [parsed]
    } else {
      return { items: null, error: 'JSON 格式无效' }
    }

    const validItems: TokenJsonItem[] = []
    for (const item of rawItems) {
      // CLIProxyAPI 导出：跳过 type 非空且非 "kiro" 的条目（如 gemini/openai）
      if (item && typeof item === 'object') {
        const t = (item as Record<string, unknown>).type
        if (typeof t === 'string' && t.trim() && t.trim().toLowerCase() !== 'kiro') continue
      }
      const normalized = normalizeKamAccount(item)
      if (!normalized || typeof normalized !== 'object') continue
      const obj = normalized as Record<string, unknown>

      // KAM 嵌套格式：{ credentials: { refreshToken, ... } }
      if (obj.credentials && typeof obj.credentials === 'object') {
        const flat = flattenKamAccount(obj)
        if (flat) validItems.push(flat)
        continue
      }

      // api_key 扁平格式：{ kiroApiKey / apiKey / kiro_api_key, ... }（无 refreshToken）
      const apiKeyRaw = obj.kiroApiKey ?? obj.apiKey ?? obj.kiro_api_key
      if (typeof apiKeyRaw === 'string' && apiKeyRaw.trim()) {
        const tokenItem = { ...obj, kiroApiKey: apiKeyRaw.trim(), authMethod: 'api_key' } as TokenJsonItem
        if (!tokenItem.region && obj.authRegion) {
          tokenItem.region = obj.authRegion as string
        }
        validItems.push(tokenItem)
        continue
      }

      // 扁平格式：{ refreshToken, ... }
      if (typeof obj.refreshToken === 'string' && obj.refreshToken.trim()) {
        const tokenItem = { ...obj, refreshToken: obj.refreshToken.trim() } as TokenJsonItem
        // 兼容旧批量导入的 authRegion 字段
        if (!tokenItem.region && obj.authRegion) {
          tokenItem.region = obj.authRegion as string
        }
        if (!tokenItem.authMethod && tokenItem.clientId && tokenItem.clientSecret) {
          tokenItem.authMethod = 'idc'
        }
        validItems.push(tokenItem)
      }
    }

    if (validItems.length === 0) {
      return { items: null, error: 'JSON 中没有找到有效的凭据（需要包含 refreshToken 或 kiroApiKey 字段）' }
    }
    return { items: validItems, error: null }
  }, [flattenKamAccount, normalizeKamAccount])

  // 解析 JSON（兼容 Token JSON / KAM 导出 / 批量导入格式）；失败时弹 toast
  const parseJson = useCallback((text: string): TokenJsonItem[] | null => {
    const { items, error } = parseJsonCore(text)
    if (error) toast.error(error)
    return items
  }, [parseJsonCore])

  // 静默解析（多文件导入时用，不弹 toast，由调用方汇总）
  const parseJsonSilent = useCallback((text: string): TokenJsonItem[] | null => {
    return parseJsonCore(text).items
  }, [parseJsonCore])

  const readJsonFiles = useCallback(async (files: FileList | File[]) => {
    const jsonFiles = Array.from(files).filter(file => file.name.endsWith('.json'))
    if (jsonFiles.length === 0) {
      toast.error('请上传 JSON 文件')
      return
    }
    if (jsonFiles.length !== Array.from(files).length) {
      toast.error('仅支持 JSON 文件')
      return
    }

    // 逐文件读取 + 解析：单个文件失败不影响其它文件（容错），
    // 最后汇总成功/失败数量，并把所有成功解析的凭据合并去重后填入文本框。
    const readOne = (file: File) =>
      new Promise<string>((resolve, reject) => {
        const reader = new FileReader()
        reader.onload = (event) => resolve((event.target?.result as string) || '')
        reader.onerror = () => reject(new Error(`${file.name} 读取失败`))
        reader.readAsText(file)
      })

    const mergedItems: TokenJsonItem[] = []
    const seen = new Set<string>() // 按 refreshToken 去重，避免多文件/文件内重复
    const failedFiles: string[] = []

    for (const file of jsonFiles) {
      try {
        const content = await readOne(file)
        // parseJson 内部会在失败时弹 toast；此处静默解析、自行汇总
        const items = parseJsonSilent(content)
        if (!items || items.length === 0) {
          failedFiles.push(file.name)
          continue
        }
        for (const it of items) {
          const key = it.refreshToken ?? ''
          if (key && seen.has(key)) continue
          if (key) seen.add(key)
          mergedItems.push(it)
        }
      } catch {
        failedFiles.push(file.name)
      }
    }

    if (mergedItems.length === 0) {
      toast.error(`未从 ${jsonFiles.length} 个文件解析出有效凭据`)
      return
    }

    setJsonText(JSON.stringify(mergedItems, null, 2))
    if (failedFiles.length > 0) {
      toast.warning(
        `已载入 ${mergedItems.length} 条凭据（${jsonFiles.length - failedFiles.length}/${jsonFiles.length} 个文件成功）；` +
        `失败：${failedFiles.join('、')}`
      )
    } else {
      toast.success(`已载入 ${jsonFiles.length} 个 JSON 文件，共 ${mergedItems.length} 条凭据`)
    }
  }, [parseJsonSilent])

  // 文件拖放
  const handleDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault()
    setIsDragging(false)
    void readJsonFiles(e.dataTransfer.files)
  }, [readJsonFiles])

  // 文件选择
  const handleFileSelect = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const files = e.target.files
    if (!files || files.length === 0) return
    void readJsonFiles(files)
    e.target.value = ''
  }, [readJsonFiles])

  // 预览（dry-run）
  const handlePreview = useCallback(() => {
    const items = parseJson(jsonText)
    if (!items) return
    setParsedItems(items)
    importMutate(
      { dryRun: true, items },
      {
        onSuccess: (response) => {
          setPreviewResults(response.items)
          setPreviewSummary(response.summary)
          setStep('preview')
        },
        onError: (error) => {
          toast.error(`预览失败: ${extractErrorMessage(error)}`)
        },
      }
    )
  }, [jsonText, parseJson, importMutate])

  // 回滚凭据（禁用 + 删除）
  const rollbackCredential = async (id: number): Promise<{ success: boolean; error?: string }> => {
    try {
      await setCredentialDisabled(id, true)
    } catch (error) {
      return { success: false, error: `禁用失败: ${extractErrorMessage(error)}` }
    }
    try {
      await deleteCredential(id)
      return { success: true }
    } catch (error) {
      return { success: false, error: `删除失败: ${extractErrorMessage(error)}` }
    }
  }

  // 验活流程
  const runVerification = useCallback(async (results: ImportItemResult[]) => {
    const addedItems = results.filter(r => r.action === 'added' && r.credentialId)
    if (addedItems.length === 0) {
      toast.info('没有新增凭据需要验活')
      return
    }

    setIsVerifying(true)
    setStep('verifying')
    setVerifyProgress({ current: 0, total: addedItems.length })

    const initialVerifyResults: VerifyItemResult[] = addedItems.map(item => ({
      index: item.index,
      credentialId: item.credentialId,
      status: 'pending',
    }))
    setVerifyResults(initialVerifyResults)

    let successCount = 0
    let failCount = 0

    for (let i = 0; i < addedItems.length; i++) {
      const item = addedItems[i]
      const credId = item.credentialId!

      // 更新为验活中
      setVerifyResults(prev => prev.map((r, idx) =>
        idx === i ? { ...r, status: 'verifying' } : r
      ))

      try {
        await new Promise(resolve => setTimeout(resolve, 1000))
        const balance = await getCredentialBalance(credId)
        successCount++
        setVerifyResults(prev => prev.map((r, idx) =>
          idx === i ? { ...r, status: 'verified', usage: `${balance.currentUsage}/${balance.usageLimit}` } : r
        ))
      } catch (error) {
        failCount++
        // 验活失败，回滚
        const rollback = await rollbackCredential(credId)
        setVerifyResults(prev => prev.map((r, idx) =>
          idx === i ? {
            ...r,
            status: rollback.success ? 'rolled_back' : 'rollback_failed',
            error: extractErrorMessage(error),
            rollbackError: rollback.error,
          } : r
        ))
      }

      setVerifyProgress({ current: i + 1, total: addedItems.length })
    }

    setIsVerifying(false)

    if (failCount === 0) {
      toast.success(`全部 ${successCount} 个凭据验活成功`)
    } else {
      toast.info(`验活完成：成功 ${successCount}，失败 ${failCount}`)
    }
  }, [deleteCredential])

  // 确认导入
  const handleConfirmImport = useCallback(() => {
    importMutate(
      { dryRun: false, items: parsedItems },
      {
        onSuccess: (response) => {
          setFinalResults(response.items)
          setFinalSummary(response.summary)

          if (enableVerify) {
            // 开启验活模式：导入后自动验活
            if (response.summary.added > 0) {
              toast.success(`成功导入 ${response.summary.added} 个凭据，开始验活...`)
              runVerification(response.items)
            } else {
              // 没有新增凭据，直接显示结果
              setStep('result')
              toast.info('没有新增凭据需要验活')
            }
          } else {
            // 普通模式：直接显示结果
            setStep('result')
            if (response.summary.added > 0) {
              toast.success(`成功导入 ${response.summary.added} 个凭据`)
            }
          }
        },
        onError: (error) => {
          toast.error(`导入失败: ${extractErrorMessage(error)}`)
        },
      }
    )
  }, [parsedItems, importMutate, enableVerify, runVerification])

  // 渲染图标
  const renderActionIcon = (action: string) => {
    switch (action) {
      case 'added': return <CheckCircle2 className="h-4 w-4 text-green-500" />
      case 'skipped': return <AlertCircle className="h-4 w-4 text-yellow-500" />
      case 'invalid': return <XCircle className="h-4 w-4 text-red-500" />
      default: return null
    }
  }

  const renderActionText = (action: string) => {
    switch (action) {
      case 'added': return <span className="text-green-600">添加</span>
      case 'skipped': return <span className="text-yellow-600">跳过</span>
      case 'invalid': return <span className="text-red-600">无效</span>
      default: return action
    }
  }

  const getVerifyStatusIcon = (status: VerifyItemResult['status']) => {
    switch (status) {
      case 'pending': return <div className="w-5 h-5 rounded-full border-2 border-gray-300" />
      case 'verifying': return <Loader2 className="w-5 h-5 animate-spin text-blue-500" />
      case 'verified': return <CheckCircle2 className="w-5 h-5 text-green-500" />
      case 'failed':
      case 'rollback_failed': return <XCircle className="w-5 h-5 text-red-500" />
      case 'rolled_back': return <AlertCircle className="w-5 h-5 text-yellow-500" />
      case 'skipped': return <AlertCircle className="w-5 h-5 text-gray-400" />
    }
  }

  const getVerifyStatusText = (result: VerifyItemResult) => {
    switch (result.status) {
      case 'pending': return '等待中'
      case 'verifying': return '验活中...'
      case 'verified': return '验活成功'
      case 'failed': return '验活失败'
      case 'rolled_back': return '验活失败（已排除）'
      case 'rollback_failed': return '验活失败（未排除）'
      case 'skipped': return '跳过'
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleClose}>
      <DialogContent className="sm:max-w-2xl max-h-[80vh] overflow-hidden flex flex-col">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <FileJson className="h-5 w-5" />
            导入凭据
          </DialogTitle>
          <DialogDescription>
            {step === 'input' && '粘贴或上传 JSON 文件以批量导入凭据'}
            {step === 'preview' && '预览导入结果，确认后执行导入'}
            {step === 'result' && '导入完成'}
            {step === 'verifying' && '正在验活导入的凭据...'}
          </DialogDescription>
        </DialogHeader>

        <div className="flex-1 overflow-auto py-4">
          {/* Step 1: Input */}
          {step === 'input' && (
            <div className="space-y-4">
              {/* 拖放区域 */}
              <div
                className={`border-2 border-dashed rounded-lg p-8 text-center transition-colors ${
                  isDragging
                    ? 'border-primary bg-primary/5'
                    : 'border-muted-foreground/25 hover:border-muted-foreground/50'
                }`}
                onDragOver={(e) => { e.preventDefault(); setIsDragging(true) }}
                onDragLeave={() => setIsDragging(false)}
                onDrop={handleDrop}
                onClick={() => fileInputRef.current?.click()}
              >
                <Upload className="h-10 w-10 mx-auto mb-4 text-muted-foreground" />
                <p className="text-sm text-muted-foreground mb-2">
                  拖放一个或多个 JSON 文件到此处，或点击选择文件
                </p>
                <p className="text-xs text-muted-foreground">
                  支持单个凭据、凭据数组和多文件合并导入
                </p>
                <input
                  ref={fileInputRef}
                  type="file"
                  accept=".json"
                  multiple
                  className="hidden"
                  onChange={handleFileSelect}
                />
              </div>

              {/* 分隔线 */}
              <div className="relative">
                <div className="absolute inset-0 flex items-center">
                  <span className="w-full border-t" />
                </div>
                <div className="relative flex justify-center text-xs uppercase">
                  <span className="bg-background px-2 text-muted-foreground">或</span>
                </div>
              </div>

              {/* 文本输入 */}
              <div className="space-y-2">
                <label className="text-sm font-medium">直接粘贴 JSON</label>
                <textarea
                  className="w-full h-48 p-3 text-sm font-mono border rounded-md bg-background resize-none focus:outline-none focus:ring-2 focus:ring-ring"
                  placeholder={'粘贴 Kiro Account Manager 导出的 JSON\n\n支持 KAM 1.8.3+ 新版平铺格式：\n[\n  {\n    "email": "...",\n    "refreshToken": "...",\n    "clientId": "...",\n    "clientSecret": "...",\n    "region": "us-east-1"\n  }\n]\n\n也支持旧版嵌套格式：\n{\n  "version": "1.5.0",\n  "accounts": [\n    {\n      "email": "...",\n      "credentials": {\n        "refreshToken": "...",\n        "clientId": "...",\n        "clientSecret": "...",\n        "region": "us-east-1"\n      }\n    }\n  ]\n}'}
                  value={jsonText}
                  onChange={(e) => setJsonText(e.target.value)}
                />
              </div>
            </div>
          )}

          {/* Step 2: Preview */}
          {step === 'preview' && previewSummary && (
            <div className="space-y-4">
              {/* 统计 */}
              <div className="grid grid-cols-4 gap-4">
                <div className="text-center p-3 bg-muted rounded-lg">
                  <div className="text-2xl font-bold">{previewSummary.parsed}</div>
                  <div className="text-xs text-muted-foreground">解析</div>
                </div>
                <div className="text-center p-3 bg-green-50 dark:bg-green-950 rounded-lg">
                  <div className="text-2xl font-bold text-green-600">{previewSummary.added}</div>
                  <div className="text-xs text-muted-foreground">将添加</div>
                </div>
                <div className="text-center p-3 bg-yellow-50 dark:bg-yellow-950 rounded-lg">
                  <div className="text-2xl font-bold text-yellow-600">{previewSummary.skipped}</div>
                  <div className="text-xs text-muted-foreground">跳过</div>
                </div>
                <div className="text-center p-3 bg-red-50 dark:bg-red-950 rounded-lg">
                  <div className="text-2xl font-bold text-red-600">{previewSummary.invalid}</div>
                  <div className="text-xs text-muted-foreground">无效</div>
                </div>
              </div>

              {/* 预览列表 */}
              <div className="border rounded-lg overflow-hidden">
                <div className="max-h-48 overflow-auto">
                  <table className="w-full text-sm">
                    <thead className="bg-muted sticky top-0">
                      <tr>
                        <th className="text-left p-2 font-medium">#</th>
                        <th className="text-left p-2 font-medium">指纹</th>
                        <th className="text-left p-2 font-medium">状态</th>
                        <th className="text-left p-2 font-medium">原因</th>
                      </tr>
                    </thead>
                    <tbody>
                      {previewResults.map((item) => (
                        <tr key={item.index} className="border-t">
                          <td className="p-2">{item.index + 1}</td>
                          <td className="p-2 font-mono text-xs">{item.fingerprint}</td>
                          <td className="p-2">
                            <div className="flex items-center gap-1">
                              {renderActionIcon(item.action)}
                              {renderActionText(item.action)}
                            </div>
                          </td>
                          <td className="p-2 text-muted-foreground text-xs">
                            {item.reason || '-'}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>

              {/* 验活开关 */}
              {previewSummary.added > 0 && (
                <div className="flex items-center justify-between p-3 border rounded-lg bg-muted/50">
                  <div>
                    <div className="text-sm font-medium">导入后自动验活</div>
                    <div className="text-xs text-muted-foreground">
                      逐个检查凭据有效性，失败的自动排除
                    </div>
                  </div>
                  <Switch checked={enableVerify} onCheckedChange={setEnableVerify} />
                </div>
              )}
            </div>
          )}

          {/* Step 3: Result (普通模式) */}
          {step === 'result' && finalSummary && (
            <div className="space-y-4">
              <div className="grid grid-cols-4 gap-4">
                <div className="text-center p-3 bg-muted rounded-lg">
                  <div className="text-2xl font-bold">{finalSummary.parsed}</div>
                  <div className="text-xs text-muted-foreground">解析</div>
                </div>
                <div className="text-center p-3 bg-green-50 dark:bg-green-950 rounded-lg">
                  <div className="text-2xl font-bold text-green-600">{finalSummary.added}</div>
                  <div className="text-xs text-muted-foreground">已添加</div>
                </div>
                <div className="text-center p-3 bg-yellow-50 dark:bg-yellow-950 rounded-lg">
                  <div className="text-2xl font-bold text-yellow-600">{finalSummary.skipped}</div>
                  <div className="text-xs text-muted-foreground">跳过</div>
                </div>
                <div className="text-center p-3 bg-red-50 dark:bg-red-950 rounded-lg">
                  <div className="text-2xl font-bold text-red-600">{finalSummary.invalid}</div>
                  <div className="text-xs text-muted-foreground">无效</div>
                </div>
              </div>

              <div className="border rounded-lg overflow-hidden">
                <div className="max-h-64 overflow-auto">
                  <table className="w-full text-sm">
                    <thead className="bg-muted sticky top-0">
                      <tr>
                        <th className="text-left p-2 font-medium">#</th>
                        <th className="text-left p-2 font-medium">指纹</th>
                        <th className="text-left p-2 font-medium">状态</th>
                        <th className="text-left p-2 font-medium">凭据 ID</th>
                      </tr>
                    </thead>
                    <tbody>
                      {finalResults.map((item) => (
                        <tr key={item.index} className="border-t">
                          <td className="p-2">{item.index + 1}</td>
                          <td className="p-2 font-mono text-xs">{item.fingerprint}</td>
                          <td className="p-2">
                            <div className="flex items-center gap-1">
                              {renderActionIcon(item.action)}
                              {renderActionText(item.action)}
                            </div>
                          </td>
                          <td className="p-2">
                            {item.credentialId ? `#${item.credentialId}` : item.reason || '-'}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            </div>
          )}

          {/* Step: Verifying (验活模式) */}
          {step === 'verifying' && (
            <div className="space-y-4">
              {/* 进度条 */}
              <div className="space-y-2">
                <div className="flex justify-between text-sm">
                  <span>{isVerifying ? '验活进度' : '验活完成'}</span>
                  <span>{verifyProgress.current} / {verifyProgress.total}</span>
                </div>
                <div className="w-full bg-secondary rounded-full h-2">
                  <div
                    className="bg-primary h-2 rounded-full transition-all"
                    style={{ width: verifyProgress.total > 0 ? `${(verifyProgress.current / verifyProgress.total) * 100}%` : '0%' }}
                  />
                </div>
              </div>

              {/* 统计 */}
              <div className="flex gap-4 text-sm">
                <span className="text-green-600 dark:text-green-400">
                  ✓ 成功: {verifyResults.filter(r => r.status === 'verified').length}
                </span>
                <span className="text-yellow-600 dark:text-yellow-400">
                  ⚠ 已排除: {verifyResults.filter(r => r.status === 'rolled_back').length}
                </span>
                <span className="text-red-600 dark:text-red-400">
                  ✗ 失败: {verifyResults.filter(r => r.status === 'rollback_failed').length}
                </span>
              </div>

              {/* 结果列表 */}
              <div className="border rounded-md divide-y max-h-[300px] overflow-y-auto">
                {verifyResults.map((result) => (
                  <div key={result.index} className="p-3">
                    <div className="flex items-start gap-3">
                      {getVerifyStatusIcon(result.status)}
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="text-sm font-medium">
                            凭据 #{result.credentialId || result.index + 1}
                          </span>
                          <span className="text-xs text-muted-foreground">
                            {getVerifyStatusText(result)}
                          </span>
                        </div>
                        {result.usage && (
                          <div className="text-xs text-muted-foreground mt-1">
                            用量: {result.usage}
                          </div>
                        )}
                        {result.error && (
                          <div className="text-xs text-red-600 dark:text-red-400 mt-1">
                            {result.error}
                          </div>
                        )}
                        {result.rollbackError && (
                          <div className="text-xs text-red-600 dark:text-red-400 mt-1">
                            回滚失败: {result.rollbackError}
                          </div>
                        )}
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>

        <DialogFooter>
          {step === 'input' && (
            <>
              <Button variant="outline" onClick={handleClose}>
                取消
              </Button>
              <Button onClick={handlePreview} disabled={!jsonText.trim() || isPending}>
                {isPending ? (
                  <>
                    <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                    解析中...
                  </>
                ) : (
                  '预览'
                )}
              </Button>
            </>
          )}

          {step === 'preview' && (
            <>
              <Button variant="outline" onClick={() => setStep('input')}>
                返回
              </Button>
              <Button
                onClick={handleConfirmImport}
                disabled={isPending || (previewSummary?.added ?? 0) === 0}
              >
                {isPending ? (
                  <>
                    <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                    导入中...
                  </>
                ) : enableVerify ? (
                  `导入并验活 (${previewSummary?.added ?? 0})`
                ) : (
                  `确认导入 (${previewSummary?.added ?? 0})`
                )}
              </Button>
            </>
          )}

          {step === 'result' && (
            <Button onClick={handleClose}>完成</Button>
          )}

          {step === 'verifying' && (
            <Button onClick={handleClose} disabled={isVerifying}>
              {isVerifying ? '验活中...' : '完成'}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
