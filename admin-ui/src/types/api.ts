// 凭据状态响应
export interface CredentialsStatusResponse {
  total: number
  available: number
  credentials: CredentialStatusItem[]
}

// 单个凭据状态
export interface CredentialStatusItem {
  id: number
  priority: number
  disabled: boolean
  failureCount: number
  refreshFailureCount: number
  disabledReason?: string | null
  expiresAt: string | null
  authMethod: string | null
  hasProfileArn: boolean
  accountEmail: string | null
  email?: string
  refreshTokenHash?: string
  subscriptionTitle?: string | null

  // ===== 统计（可持久化） =====
  callsTotal: number
  callsOk: number
  callsErr: number
  inputTokensTotal: number
  outputTokensTotal: number
  lastCallAt: string | null
  lastSuccessAt: string | null
  lastErrorAt: string | null
  lastError: string | null

  // ===== upstream 字段 =====
  successCount: number
  lastUsedAt: string | null
  hasProxy: boolean
  /** 凭据级 Region（用于 Token 刷新） */
  region: string | null
  /** 凭据级 API Region（单独覆盖 API 请求） */
  apiRegion: string | null
  /** 凭据显式配置的 endpoint，null 表示回退默认值 */
  endpoint?: string | null
  /** 最终生效的 endpoint */
  effectiveEndpoint: string
  /** Web Portal Idp 标识（默认推断为 Google） */
  idp?: string | null
  /** 凭据级代理 URL（null 表示回退到全局代理；"direct" 表示显式直连） */
  proxyUrl?: string | null
  /** 凭据级代理用户名 */
  proxyUsername?: string | null
  /** 是否设置了凭据级代理密码（不返回明文） */
  hasProxyPassword?: boolean
  /** 最近一次已知的超额开关状态（null 表示未知） */
  overageEnabled?: boolean | null
  /** 是否正在执行后台开启超额任务 */
  overageEnabling?: boolean
  /** 最近一次开启超额失败原因 */
  overageLastError?: string | null
}

// 余额响应
export interface BalanceResponse {
  id: number
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  remaining: number
  usagePercentage: number
  nextResetAt: number | null
  overageEnabled: boolean
  overageCap: number
  /** 订阅超额能力标识（"OVERAGE_CAPABLE" 表示该套餐支持开启超额） */
  overageCapability?: string | null
}

// 缓存余额信息
export interface CachedBalanceInfo {
  id: number
  remaining: number
  usageLimit: number
  usagePercentage: number
  subscriptionTitle: string | null
  cachedAt: number // Unix 毫秒时间戳
  ttlSecs: number
  /** 缓存快照里的上游超额状态 */
  overageEnabled?: boolean
  /** 缓存快照里的超额额度上限 */
  overageCap?: number
  /** 缓存快照里的订阅超额能力标识 */
  overageCapability?: string | null
}

// 缓存余额响应
export interface CachedBalancesResponse {
  balances: CachedBalanceInfo[]
}

// 成功响应
export interface SuccessResponse {
  success: boolean
  message: string
}

// ===== 统计（可持久化） =====

export interface StatsBucket {
  // 按日：YYYY-MM-DD；按模型：model id
  key: string
  callsTotal: number
  callsOk: number
  callsErr: number
  inputTokensTotal: number
  outputTokensTotal: number
  lastCallAt: string | null
  lastSuccessAt: string | null
  lastErrorAt: string | null
  lastError: string | null
}

export interface CredentialStatsResponse {
  id: number
  callsTotal: number
  callsOk: number
  callsErr: number
  inputTokensTotal: number
  outputTokensTotal: number
  lastCallAt: string | null
  lastSuccessAt: string | null
  lastErrorAt: string | null
  lastError: string | null
  byDay: StatsBucket[]
  byModel: StatsBucket[]
}

// 错误响应
export interface AdminErrorResponse {
  error: {
    type: string
    message: string
  }
}

// 请求类型
export interface SetDisabledRequest {
  disabled: boolean
}

export interface SetPriorityRequest {
  priority: number
}

export interface SetEndpointRequest {
  endpoint: string | null
}

export interface SetIdpRequest {
  idp: string | null
}

export interface SetCredentialProxyRequest {
  proxyUrl: string | null
  proxyUsername?: string | null
  proxyPassword?: string | null
}

export interface OverageStatusResponse {
  id: number
  enabled: boolean | null
  enabling: boolean
  lastError: string | null
  hasProfileArn: boolean
  authMethod: string | null
}

export type OverageEvent =
  | { kind: 'prepared'; idp: string; hasProfileArn: boolean }
  | { kind: 'submittingUpdate' }
  | { kind: 'updateAccepted' }
  | { kind: 'pollingStarted'; intervalMs: number; timeoutMs: number }
  | { kind: 'pollTick'; attempt: number; overageEnabled: boolean | null; elapsedMs: number }
  | { kind: 'done'; overageEnabled: boolean }
  | { kind: 'error'; message: string }

// 添加凭据请求
export interface AddCredentialRequest {
  refreshToken?: string
  kiroApiKey?: string
  authMethod?: 'social' | 'idc' | 'api_key'
  clientId?: string
  clientSecret?: string
  priority?: number
  /** Region（用于 Token 刷新及默认 API 请求），可被 apiRegion 单独覆盖 */
  region?: string
  /** 单独覆盖 API 请求使用的 region */
  apiRegion?: string
  machineId?: string
  endpoint?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
}

// 添加凭据响应
export interface AddCredentialResponse {
  success: boolean
  message: string
  credentialId: number
  email?: string
}

// ===== 账号信息（套餐/用量/邮箱等） =====

export interface CreditBonus {
  code: string
  name: string
  current: number
  limit: number
  expiresAt: string | null
}

export interface CreditsResourceDetail {
  displayName: string | null
  displayNamePlural: string | null
  resourceType: string | null
  currency: string | null
  unit: string | null
  overageRate: number | null
  overageCap: number | null
}

export interface CreditsUsageSummary {
  current: number
  limit: number
  baseCurrent: number
  baseLimit: number
  freeTrialCurrent: number
  freeTrialLimit: number
  freeTrialExpiry: string | null
  bonuses: CreditBonus[]
  /** epoch 秒（可能带小数），与 Kiro 上游 `nextDateReset` 一致 */
  nextResetDate: number | null
  overageEnabled: boolean | null
  resourceDetail: CreditsResourceDetail | null
}

export interface AccountSubscriptionDetails {
  rawType: string | null
  managementTarget: string | null
  upgradeCapability: string | null
  overageCapability: string | null
}

export interface ResourceUsageSummary {
  resourceType: string | null
  displayName: string | null
  unit: string | null
  currency: string | null
  current: number
  limit: number
}

export interface UsageAndLimitsResponse {
  userInfo: { email: string | null; userId: string | null } | null
  subscriptionInfo:
    | {
        type: string | null
        subscriptionTitle: string | null
        upgradeCapability: string | null
        overageCapability: string | null
        subscriptionManagementTarget: string | null
      }
    | null
  usageBreakdownList:
    | Array<{
        resourceType: string | null
        currentUsage: number | null
        currentUsageWithPrecision: number | null
        usageLimit: number | null
        usageLimitWithPrecision: number | null
        displayName: string | null
        displayNamePlural: string | null
        currency: string | null
        unit: string | null
        overageRate: number | null
        overageCap: number | null
        freeTrialInfo:
          | {
              usageLimit: number | null
              usageLimitWithPrecision: number | null
              currentUsage: number | null
              currentUsageWithPrecision: number | null
              freeTrialExpiry: string | null
              freeTrialStatus: string | null
            }
          | null
        bonuses:
          | Array<{
              bonusCode: string | null
              displayName: string | null
              usageLimit: number | null
              usageLimitWithPrecision: number | null
              currentUsage: number | null
              currentUsageWithPrecision: number | null
              status: string | null
              expiresAt: string | null
            }>
          | null
      }>
    | null
  /** epoch 秒（可能带小数），与 Kiro 上游 `nextDateReset` 一致 */
  nextDateReset: number | null
  overageConfiguration: { overageEnabled: boolean | null } | null
}

export interface AccountAggregateInfo {
  email: string | null
  userId: string | null
  idp: string | null
  status: string | null
  featureFlags: string[] | null
  subscriptionTitle: string | null
  subscriptionType: string
  subscription: AccountSubscriptionDetails
  usage: CreditsUsageSummary
  resources: ResourceUsageSummary[]
  rawUsage: UsageAndLimitsResponse
}

export interface CredentialAccountInfoResponse {
  id: number
  account: AccountAggregateInfo
}

// ============ 批量导入 token.json ============

// 官方 token.json 格式（用于解析导入）
export interface TokenJsonItem {
  provider?: string
  refreshToken?: string
  clientId?: string
  clientSecret?: string
  authMethod?: string
  priority?: number
  region?: string
  machineId?: string
}

// 批量导入请求
export interface ImportTokenJsonRequest {
  dryRun?: boolean
  items: TokenJsonItem | TokenJsonItem[]
}

// 导入动作
export type ImportAction = 'added' | 'skipped' | 'invalid'

// 单项导入结果
export interface ImportItemResult {
  index: number
  fingerprint: string
  action: ImportAction
  reason?: string
  credentialId?: number
}

// 导入汇总
export interface ImportSummary {
  parsed: number
  added: number
  skipped: number
  invalid: number
}

// 批量导入响应
export interface ImportTokenJsonResponse {
  summary: ImportSummary
  items: ImportItemResult[]
}

// ============ 全局代理配置 ============

export interface ProxyConfigResponse {
  proxyUrl: string | null
  hasCredentials: boolean
}

export interface UpdateProxyConfigRequest {
  proxyUrl?: string | null
  proxyUsername?: string | null
  proxyPassword?: string | null
}

// ============ 全局配置 ============

export interface CompressionConfigResponse {
  enabled: boolean
  whitespaceCompression: boolean
  thinkingStrategy: string
  toolResultMaxChars: number
  toolResultHeadLines: number
  toolResultTailLines: number
  toolUseInputMaxChars: number
  toolDescriptionMaxChars: number
  maxHistoryTurns: number
  maxHistoryChars: number
  maxRequestBodyBytes: number
}

export interface GlobalConfigResponse {
  region: string
  credentialRpm: number | null
  promptCacheTtlSeconds: number
  promptCacheAccountingEnabled: boolean
  defaultEndpoint: string
  compression: CompressionConfigResponse
}

export interface UpdateCompressionConfigRequest {
  enabled?: boolean
  whitespaceCompression?: boolean
  thinkingStrategy?: string
  toolResultMaxChars?: number
  toolResultHeadLines?: number
  toolResultTailLines?: number
  toolUseInputMaxChars?: number
  toolDescriptionMaxChars?: number
  maxHistoryTurns?: number
  maxHistoryChars?: number
  maxRequestBodyBytes?: number
}

export interface UpdateGlobalConfigRequest {
  region?: string
  credentialRpm?: number | null
  promptCacheTtlSeconds?: number
  promptCacheAccountingEnabled?: boolean
  defaultEndpoint?: string
  compression?: UpdateCompressionConfigRequest
}
