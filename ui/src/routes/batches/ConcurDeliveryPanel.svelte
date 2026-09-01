<script lang="ts">
  import { describeError, invokeSafe } from '../../lib/ipc'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'
  import type {
    Batch, BatchReviewSnapshot, ConcurDraftCapability, ConcurMappingProfile,
    ConcurUploadPreflight, ConcurUploadSession, ConcurUploadStatus, ExpenseItem,
    PaymentMethod,
  } from '../../lib/types'
  import { EXPENSE_CATEGORIES, EXPENSE_CATEGORY_LABELS } from '../../lib/types'

  interface Props {
    batchId: number
    batch: Batch
    snapshot: BatchReviewSnapshot
    onBackToReview: () => void
  }
  let { batchId, batch, snapshot, onBackToReview }: Props = $props()

  const PAYMENT_LABELS: Record<PaymentMethod, string> = {
    unknown: '待确认', personal_card: '个人卡', corporate_card: '公司卡', cash: '现金', other: '其他',
  }
  const CORE_REQUIRED = ['expense_type', 'transaction_date', 'vendor_name', 'purchase_city', 'amount', 'vat_amount', 'vat_rate']
  const FIELD_RULES = {
    expense_type: { source: 'ExpenseItem.category_code', transform: 'expense_type_option_lookup' },
    transaction_date: { source: 'ExpenseItem.transaction_date', transform: 'date_iso' },
    business_purpose: { source: 'ExpenseItem.description', transform: 'trim' },
    vendor_name: { source: 'ExpenseItem.counterparty_name', transform: 'trim' },
    purchase_city: { source: 'ExpenseItem.location.city_name', transform: 'location_option_lookup' },
    payment_type: { source: 'ExpenseItem.payment_method', transform: 'payment_option_lookup' },
    amount: { source: 'ExpenseItem.gross_amount', transform: 'decimal_exact' },
    currency: { source: 'ExpenseItem.currency_code', transform: 'iso_currency' },
    vat_amount: { source: 'ExpenseItem.tax_details[].amount', transform: 'sum_tax_amount' },
    vat_rate: { source: 'ExpenseItem.tax_details[].rate', transform: 'vat_option_lookup' },
    attachments: { source: 'ExpenseItem.documents[]', transform: 'attach_to_same_expense' },
  }

  let profiles = $state<ConcurMappingProfile[]>([])
  let expenses = $state<ExpenseItem[]>([])
  let sessions = $state<ConcurUploadSession[]>([])
  let selectedProfileId = $state('')
  let loading = $state(true)
  let saving = $state(false)
  let checking = $state(false)
  let showProfileEditor = $state(false)
  let error = $state<string | null>(null)
  let notice = $state<string | null>(null)
  let preflight = $state<ConcurUploadPreflight | null>(null)
  let capability = $state<ConcurDraftCapability | null>(null)
  let selectedSessionStatus = $state<ConcurUploadStatus | null>(null)
  let verificationConfirmation = $state<{ kind: 'report' | 'expense' | 'attachment'; objectId: number } | null>(null)
  let statusLoading = $state(false)
  let resolving = $state(false)
  let startingDelivery = $state(false)
  let resolutionExternalIds = $state<Record<string, string>>({})
  let uploadOverrides = $state<Record<string, Record<string, string>>>({})

  let profileId = $state<number | null>(null)
  let profileName = $state('')
  let companyLabel = $state('')
  let adapterKind = $state<'ui_assisted' | 'api'>('ui_assisted')
  let categoryMap = $state<Record<string, string>>({})
  let locationMap = $state<Record<string, string>>({})
  let paymentMap = $state<Record<string, string>>({})
  let vatRateMap = $state<Record<string, string>>({})
  let paymentRequired = $state(false)
  let extraRequiredFields = $state('')
  let reportCustomFieldsJson = $state('{}')
  let expenseCustomFieldsJson = $state('{}')

  let reportName = $state('')
  let reportDate = $state(new Date().toISOString().slice(0, 10))
  let reportComment = $state('')

  const cities = $derived(Array.from(new Set(expenses.map((item) => item.location.city_name).filter((value): value is string => Boolean(value)))).sort())
  const vatRates = $derived(Array.from(new Set(expenses.flatMap((item) => item.tax_details.map((tax) => tax.rate).filter((value): value is string => Boolean(value))))).sort())
  const selectedProfile = $derived(profiles.find((profile) => String(profile.id) === selectedProfileId) ?? null)
  const mappingGapCount = $derived(preflight?.gaps.filter((gap) => gap.scope === 'mapping_profile').length ?? 0)
  const factGapCount = $derived(preflight?.gaps.filter((gap) => gap.scope === 'expense_fact').length ?? 0)

  function objectJson(value: string): Record<string, string> {
    try {
      const parsed: unknown = JSON.parse(value)
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) return parsed as Record<string, string>
    } catch { /* 由后端在保存时给出准确错误 */ }
    return {}
  }

  function expenseForId(expenseItemId: number): ExpenseItem | null {
    return expenses.find((expense) => expense.id === expenseItemId) ?? null
  }

  function resetProfileEditor() {
    profileId = null
    profileName = ''
    companyLabel = ''
    adapterKind = 'ui_assisted'
    categoryMap = Object.fromEntries(EXPENSE_CATEGORIES.map((type) => [type, '']))
    locationMap = Object.fromEntries(cities.map((city) => [city, '']))
    paymentMap = Object.fromEntries(Object.keys(PAYMENT_LABELS).map((key) => [key, '']))
    vatRateMap = Object.fromEntries(vatRates.map((rate) => [rate, '']))
    paymentRequired = false
    extraRequiredFields = ''
    reportCustomFieldsJson = '{}'
    expenseCustomFieldsJson = '{}'
  }

  function editProfile(profile: ConcurMappingProfile) {
    profileId = profile.id
    profileName = profile.name
    companyLabel = profile.company_label
    adapterKind = profile.adapter_kind
    categoryMap = { ...Object.fromEntries(EXPENSE_CATEGORIES.map((type) => [type, ''])), ...objectJson(profile.expense_type_map_json) }
    locationMap = { ...Object.fromEntries(cities.map((city) => [city, ''])), ...objectJson(profile.location_map_json) }
    paymentMap = { ...Object.fromEntries(Object.keys(PAYMENT_LABELS).map((key) => [key, ''])), ...objectJson(profile.payment_type_map_json) }
    vatRateMap = { ...Object.fromEntries(vatRates.map((rate) => [rate, ''])), ...objectJson(profile.vat_rate_map_json) }
    try {
      const required = JSON.parse(profile.required_fields_json) as string[]
      paymentRequired = required.includes('payment_type')
      extraRequiredFields = required.filter((field) => !CORE_REQUIRED.includes(field) && field !== 'payment_type').join(', ')
    } catch { paymentRequired = false; extraRequiredFields = '' }
    try {
      const custom = JSON.parse(profile.custom_fields_json) as Record<string, unknown>
      const hasSections = 'report_fields' in custom || 'expense_fields' in custom
      reportCustomFieldsJson = JSON.stringify((custom.report_fields as Record<string, unknown> | undefined) ?? {}, null, 2)
      expenseCustomFieldsJson = JSON.stringify((custom.expense_fields as Record<string, unknown> | undefined) ?? (hasSections ? {} : custom), null, 2)
    } catch { reportCustomFieldsJson = '{}'; expenseCustomFieldsJson = '{}' }
    showProfileEditor = true
    preflight = null
  }

  function compactMap(value: Record<string, string>): Record<string, string> {
    return Object.fromEntries(Object.entries(value).filter(([, target]) => target.trim().length > 0).map(([source, target]) => [source, target.trim()]))
  }

  function overrideFieldKey(fieldKey: string): string {
    return {
      expense_type: 'expense_type_id', purchase_city: 'purchase_city_id', payment_type: 'payment_type_id',
      vat_amount: 'vat_amount', vat_rate: 'vat_rate_ids',
    }[fieldKey] ?? fieldKey
  }

  function updateOverride(expenseItemId: number, fieldKey: string, value: string) {
    const itemKey = String(expenseItemId)
    uploadOverrides[itemKey] = { ...(uploadOverrides[itemKey] ?? {}), [overrideFieldKey(fieldKey)]: value }
  }

  function serializedOverrides(): string {
    const result: Record<string, Record<string, string | string[]>> = {}
    for (const [expenseId, fields] of Object.entries(uploadOverrides)) {
      const values: Record<string, string | string[]> = {}
      for (const [key, value] of Object.entries(fields)) {
        if (!value.trim()) continue
        values[key] = key === 'vat_rate_ids'
          ? value.split(',').map((item) => item.trim()).filter(Boolean)
          : value.trim()
      }
      if (Object.keys(values).length > 0) result[expenseId] = values
    }
    return JSON.stringify(result)
  }

  function uploadStatusLabel(status: string): string {
    return {
      preflight: '预检有缺口', ready: '等待外部写入', running: '正在写入', partial: '部分完成',
      draft_created: '草稿已创建', needs_verification: '需要人工核对', failed: '失败，可安全重试',
      pending: '等待处理', created: '费用已创建', uploaded: '附件已上传',
    }[status] ?? status
  }

  function resolutionKey(kind: string, id: number): string { return `${kind}-${id}` }

  async function refreshSessions() {
    const result = await invokeSafe<ConcurUploadSession[]>('list_concur_upload_sessions', { batchId })
    if (result.ok) sessions = result.data
  }

  async function viewSession(sessionId: number) {
    statusLoading = true
    error = null
    const result = await invokeSafe<ConcurUploadStatus | null>('get_concur_upload_status', { sessionId })
    statusLoading = false
    if (!result.ok) { error = describeError(result.error); return }
    if (!result.data) { error = 'Concur 上传会话不存在'; return }
    selectedSessionStatus = result.data
  }

  async function resolveVerification(kind: 'report' | 'expense' | 'attachment', objectId: number, existsInConcur: boolean) {
    if (!selectedSessionStatus || resolving) return
    const key = resolutionKey(kind, objectId)
    const externalId = resolutionExternalIds[key]?.trim() ?? ''
    if (existsInConcur && !externalId) { error = '确认已存在时，请先填写在 Concur 中核对到的对象 ID'; return }
    verificationConfirmation = null
    resolving = true
    error = null
    notice = null
    const result = await invokeSafe<ConcurUploadStatus>('resolve_concur_upload_verification', {
      input: {
        session_id: selectedSessionStatus.session.id,
        object_kind: kind,
        object_id: objectId,
        exists_in_concur: existsInConcur,
        external_id: existsInConcur ? externalId : null,
      },
    })
    resolving = false
    if (!result.ok) { error = describeError(result.error); return }
    selectedSessionStatus = result.data
    notice = existsInConcur ? '已记录 Concur 外部 ID，不会重复创建。' : '已标记外部对象不存在；该项可在适配器启用后安全重试。'
    await refreshSessions()
  }

  function requestVerificationResolution(kind: 'report' | 'expense' | 'attachment', objectId: number, existsInConcur: boolean) {
    if (existsInConcur) void resolveVerification(kind, objectId, true)
    else verificationConfirmation = { kind, objectId }
  }

  async function load() {
    loading = true
    if (!reportName) reportName = batch.name
    error = null
    const [profilesResult, expensesResult, sessionsResult, capabilityResult] = await Promise.all([
      invokeSafe<ConcurMappingProfile[]>('list_concur_mapping_profiles'),
      invokeSafe<ExpenseItem[]>('list_expense_items', { batchId }),
      invokeSafe<ConcurUploadSession[]>('list_concur_upload_sessions', { batchId }),
      invokeSafe<ConcurDraftCapability>('get_concur_draft_capability'),
    ])
    loading = false
    if (!profilesResult.ok) { error = describeError(profilesResult.error); return }
    if (!expensesResult.ok) { error = describeError(expensesResult.error); return }
    if (!sessionsResult.ok) { error = describeError(sessionsResult.error); return }
    if (!capabilityResult.ok) { error = describeError(capabilityResult.error); return }
    profiles = profilesResult.data
    expenses = expensesResult.data
    sessions = sessionsResult.data
    capability = capabilityResult.data
    if (!selectedProfileId && profiles[0]) selectedProfileId = String(profiles[0].id)
    if (profiles.length === 0) { resetProfileEditor(); showProfileEditor = true }
  }

  async function saveProfile() {
    saving = true
    error = null
    notice = null
    let reportCustomFields: unknown
    let expenseCustomFields: unknown
    try {
      reportCustomFields = JSON.parse(reportCustomFieldsJson)
      expenseCustomFields = JSON.parse(expenseCustomFieldsJson)
    } catch { saving = false; error = '企业自定义字段必须是有效 JSON 对象'; return }
    if (!reportCustomFields || typeof reportCustomFields !== 'object' || Array.isArray(reportCustomFields)
      || !expenseCustomFields || typeof expenseCustomFields !== 'object' || Array.isArray(expenseCustomFields)) {
      saving = false; error = '报销单级和费用级自定义字段都必须是 JSON 对象'; return
    }
    const extraRequired = extraRequiredFields.split(/[，,\n]/).map((field) => field.trim()).filter(Boolean)
    const requiredFields = Array.from(new Set([...CORE_REQUIRED, ...(paymentRequired ? ['payment_type'] : []), ...extraRequired]))
    const result = await invokeSafe<ConcurMappingProfile>('save_concur_mapping_profile', {
      input: {
        profile_id: profileId,
        name: profileName,
        company_label: companyLabel,
        adapter_kind: adapterKind,
        field_rules_json: JSON.stringify(FIELD_RULES),
        expense_type_map_json: JSON.stringify(compactMap(categoryMap)),
        location_map_json: JSON.stringify(compactMap(locationMap)),
        payment_type_map_json: JSON.stringify(compactMap(paymentMap)),
        vat_rate_map_json: JSON.stringify(compactMap(vatRateMap)),
        required_fields_json: JSON.stringify(requiredFields),
        custom_fields_json: JSON.stringify({ report_fields: reportCustomFields, expense_fields: expenseCustomFields }),
      },
    })
    saving = false
    if (!result.ok) { error = describeError(result.error); return }
    showProfileEditor = false
    notice = `映射配置“${result.data.name}”已保存为 V${result.data.version}。`
    selectedProfileId = String(result.data.id)
    preflight = null
    await load()
  }

  async function runPreflight() {
    if (!selectedProfile) { error = '请先选择公司映射配置'; return }
    checking = true
    error = null
    notice = null
    const result = await invokeSafe<ConcurUploadPreflight>('prepare_concur_upload', {
      batchId,
      input: {
        profile_id: selectedProfile.id,
        report_name: reportName,
        report_date: reportDate,
        comment: reportComment,
        upload_overrides_json: serializedOverrides(),
      },
    })
    checking = false
    if (!result.ok) { error = describeError(result.error); return }
    preflight = result.data
    notice = result.data.ready
      ? `预检通过：将创建 1 个报销单草稿、${result.data.expenses.length} 条费用并挂载对应材料。`
      : `发现 ${result.data.gaps.length} 个缺口；本地事实与目标映射已分开列出。`
    await refreshSessions()
  }

  async function startDelivery() {
    if (!preflight?.ready || !capability?.enabled || startingDelivery) return
    startingDelivery = true
    error = null
    const result = await invokeSafe('start_concur_delivery', { batchId })
    startingDelivery = false
    if (!result.ok) { error = describeError(result.error); return }
    notice = 'Concur 草稿交付已启动；可以关闭页面，稍后从本批次会话继续查看。'
    await refreshSessions()
  }

  $effect(() => { batchId; void load() })
</script>

<section class="concur-panel" aria-labelledby="concur-panel-title">
  <header><div><span class="eyebrow">Concur 草稿交付</span><h2 id="concur-panel-title">映射与上传预检</h2><p>冻结版本 V{snapshot.version} · 不修改本地费用项 · 不执行最终提交</p></div><code>{snapshot.content_sha256.slice(0, 12)}</code></header>

  {#if loading}<p class="state">正在读取映射配置…</p>{:else}
    <div class="steps" aria-label="上传步骤"><div class="done"><span>01</span><strong>审核版本</strong><small>已冻结</small></div><div class:done={Boolean(selectedProfile)}><span>02</span><strong>公司映射</strong><small>{selectedProfile ? `V${selectedProfile.version}` : '待配置'}</small></div><div class:done={preflight?.ready}><span>03</span><strong>必填预检</strong><small>{preflight ? `${preflight.gaps.length} 个缺口` : '待执行'}</small></div><div><span>04</span><strong>创建草稿</strong><small>不自动提交</small></div></div>

    {#if error}<p class="message error" role="alert">{error}</p>{/if}
    {#if notice}<p class="message notice" role="status">{notice}</p>{/if}

    <section class="profile-section">
      <div class="section-title"><div><span>目标配置</span><h3>选择 Concur 公司映射</h3></div><div class="profile-actions"><button type="button" onclick={() => { resetProfileEditor(); showProfileEditor = true; preflight = null }}>新建配置</button>{#if selectedProfile}<button type="button" onclick={() => editProfile(selectedProfile)}>基于当前配置新建版本</button>{/if}</div></div>
      {#if profiles.length > 0}<label><span>映射配置</span><select bind:value={selectedProfileId} onchange={() => (preflight = null)}><option value="">请选择</option>{#each profiles as profile}<option value={profile.id}>{profile.company_label} · {profile.name} · V{profile.version}</option>{/each}</select></label>{/if}
    </section>

    {#if showProfileEditor}
      <section class="profile-editor">
        <header><div><span>版本化配置</span><h3>{profileId ? '保存为新版本' : '新建公司映射'}</h3></div><button type="button" onclick={() => (showProfileEditor = false)} disabled={profiles.length === 0}>收起</button></header>
        <div class="two"><label><span>配置名称 *</span><input bind:value={profileName} maxlength="100" /></label><label><span>公司/租户名称 *</span><input bind:value={companyLabel} maxlength="100" /></label></div>
        <label><span>适配方式</span><select bind:value={adapterKind}><option value="ui_assisted">浏览器辅助填报</option><option value="api">企业 API</option></select></label>
        <details open><summary>费用分类 → Concur 费用类型 ID</summary><div class="mapping-grid">{#each EXPENSE_CATEGORIES as type}<label><span>{EXPENSE_CATEGORY_LABELS[type]}</span><input bind:value={categoryMap[type]} placeholder="目标选项稳定 ID" /></label>{/each}</div></details>
        <details open={cities.length > 0}><summary>本批次城市 → Concur 地点选项 ID</summary>{#if cities.length === 0}<p>本批次尚无结构化城市。</p>{:else}<div class="mapping-grid">{#each cities as city}<label><span>{city}</span><input bind:value={locationMap[city]} placeholder="目标地点稳定 ID" /></label>{/each}</div>{/if}</details>
        <details><summary>付款方式、VAT 与额外必填项</summary><div class="mapping-grid">{#each Object.entries(PAYMENT_LABELS) as [key, label]}<label><span>{label}</span><input bind:value={paymentMap[key]} placeholder="付款选项 ID" /></label>{/each}{#each vatRates as rate}<label><span>税率 {rate}</span><input bind:value={vatRateMap[rate]} placeholder="VAT 选项 ID" /></label>{/each}</div><label class="check"><input type="checkbox" bind:checked={paymentRequired} /><span>目标租户要求付款类型必填</span></label><label><span>其他目标必填字段</span><input bind:value={extraRequiredFields} placeholder="例如 cost_center；报销单级使用 report.cost_center" /><small>多个字段用逗号分隔；预检会逐项阻止缺失值进入上传。</small></label></details>
        <details><summary>企业自定义字段（高级）</summary><p>填写目标字段 ID 与固定值，只作用于本配置版本；不会写回本地费用事实。</p><div class="two"><label><span>报销单级字段 JSON</span><textarea bind:value={reportCustomFieldsJson} placeholder="例如：costCenter 对应 CN-SALES"></textarea></label><label><span>费用级字段 JSON</span><textarea bind:value={expenseCustomFieldsJson} placeholder="例如：receiptStatus 对应 RECEIPT"></textarea></label></div></details>
        <button class="primary" type="button" onclick={saveProfile} disabled={saving || !profileName.trim() || !companyLabel.trim()}>{saving ? '正在保存…' : '保存映射版本'}</button>
      </section>
    {/if}

    <section class="report-section">
      <div class="section-title"><div><span>报销单草稿</span><h3>确认本次目标字段</h3></div><small>这些值只属于本次上传会话</small></div>
      <div class="two"><label><span>费用报告名称 *</span><input bind:value={reportName} maxlength="200" /></label><label><span>费用报告日期 *</span><input type="date" bind:value={reportDate} /></label></div>
      <label><span>Comment</span><textarea bind:value={reportComment} maxlength="500"></textarea></label>
      <button class="primary" type="button" onclick={runPreflight} disabled={checking || !selectedProfile || !reportName.trim()}>{checking ? '正在逐条映射…' : '执行上传预检'}</button>
    </section>

    {#if preflight}
      <section class="preflight" class:ready={preflight.ready}>
        <header><div><span>预检结果</span><h3>{preflight.ready ? '所有当前必填检查已通过' : `${preflight.gaps.length} 个缺口待处理`}</h3></div><div class="counts"><b>{preflight.expenses.length}</b><small>费用</small><b>{preflight.expenses.reduce((sum, item) => sum + item.attachment_document_ids.length, 0)}</b><small>材料</small></div></header>
        {#if preflight.gaps.length > 0}<div class="gap-summary"><span>本地事实 {factGapCount}</span><span>映射配置 {mappingGapCount}</span><span>其他 {preflight.gaps.length - factGapCount - mappingGapCount}</span></div><ul class="gaps">{#each preflight.gaps as gap}<li><span class:fact={gap.scope === 'expense_fact'}>{gap.scope === 'expense_fact' ? '本地事实' : gap.scope === 'mapping_profile' ? '映射配置' : gap.scope === 'attachment' ? '材料' : '上传补充'}</span><div><strong>{gap.message}</strong><small>{gap.expense_item_id ? `费用 #${gap.expense_item_id} · ` : ''}{gap.field_key}</small>{#if gap.expense_item_id && (gap.scope === 'mapping_profile' || gap.scope === 'target_override')}<input class="override" value={uploadOverrides[String(gap.expense_item_id)]?.[overrideFieldKey(gap.field_key)] ?? ''} oninput={(event) => updateOverride(Number(gap.expense_item_id), gap.field_key, event.currentTarget.value)} placeholder={gap.field_key === 'vat_rate' ? '本次目标选项 ID，多个用逗号分隔' : '仅本次上传的最终目标值'} />{/if}</div>{#if gap.resolution === 'return_to_expense_review'}<button type="button" onclick={onBackToReview}>返回审核</button>{:else if gap.resolution === 'configure_profile'}<button type="button" onclick={() => selectedProfile && editProfile(selectedProfile)}>修改映射</button>{/if}</li>{/each}</ul><button class="primary rerun" type="button" onclick={runPreflight} disabled={checking}>应用本次补充值并重新预检</button>{/if}
        <details class="projection" open={preflight.ready}><summary>查看内部来源 → Concur 冻结投影（{preflight.expenses.length}）</summary><div>{#each preflight.expenses as payload}{@const sourceExpense = expenseForId(payload.expense_item_id)}<article><header><strong>费用 #{payload.expense_item_id}</strong><span>{sourceExpense?.counterparty_name || '交易对方待补充'} · {sourceExpense?.gross_amount ?? ''} {sourceExpense?.currency_code ?? ''}</span><b>{payload.attachment_document_ids.length} 份材料</b></header><div class="projection-flow"><pre>{sourceExpense ? JSON.stringify({ category_code: sourceExpense.category_code, transaction_date: sourceExpense.transaction_date, description: sourceExpense.description, counterparty_name: sourceExpense.counterparty_name, location: sourceExpense.location, payment_method: sourceExpense.payment_method, gross_amount: sourceExpense.gross_amount, currency_code: sourceExpense.currency_code, tax_details: sourceExpense.tax_details }, null, 2) : '{}'}</pre><span>经配置 V{selectedProfile?.version ?? ''} →</span><pre>{JSON.stringify(JSON.parse(payload.target_fields_json), null, 2)}</pre></div></article>{/each}</div></details>
        {#if preflight.ready}<div class="adapter-gate"><strong>{capability?.enabled ? '可以创建 Concur 草稿' : '外部写入尚未解锁'}</strong><p>{capability?.reason ?? '正在读取适配器能力…'}</p>{#if capability && !capability.enabled}<ul>{#each capability.required_confirmations as confirmation}<li>{confirmation}</li>{/each}</ul>{/if}<button type="button" onclick={startDelivery} disabled={!capability?.enabled || startingDelivery}>{startingDelivery ? '正在启动…' : capability?.enabled ? '创建 Concur 草稿' : '创建 Concur 草稿（等待适配器验证）'}</button></div>{/if}
      </section>
    {/if}
    {#if sessions.length > 0}<details class="session-history" open={sessions.some((session) => session.status === 'needs_verification')}><summary>本批次 Concur 会话（{sessions.length}）</summary><ul>{#each sessions as session}<li class:attention={session.status === 'needs_verification'}><span>#{session.id} · 审核版本 #{session.review_snapshot_id} · 映射 V{session.mapping_profile_version}</span><strong>{uploadStatusLabel(session.status)}</strong><code>{session.idempotency_key.slice(-12)}</code><button type="button" onclick={() => viewSession(session.id)}>查看进度</button></li>{/each}</ul></details>{/if}

    {#if statusLoading}<p class="state">正在读取逐项状态…</p>{/if}
    {#if selectedSessionStatus}
      <section class="upload-status">
        <header><div><span>可恢复上传计划</span><h3>会话 #{selectedSessionStatus.session.id} · {uploadStatusLabel(selectedSessionStatus.session.status)}</h3></div><button type="button" onclick={() => (selectedSessionStatus = null)}>关闭</button></header>
        <div class="report-row"><div><strong>报销单草稿</strong><small>{selectedSessionStatus.session.external_report_id ? `Concur ID ${selectedSessionStatus.session.external_report_id}` : '尚无外部 ID'}</small></div><b>{uploadStatusLabel(selectedSessionStatus.session.status)}</b></div>
        {#if selectedSessionStatus.session.status === 'needs_verification' && !selectedSessionStatus.session.external_report_id}
          <div class="resolution"><p>程序在创建报销单时中断。请先在 Concur 中按名称和日期查找。</p><input bind:value={resolutionExternalIds[resolutionKey('report', selectedSessionStatus.session.id)]} placeholder="找到时填写 Concur 报销单 ID" /><button type="button" onclick={() => requestVerificationResolution('report', selectedSessionStatus!.session.id, true)} disabled={resolving}>确认已创建</button><button type="button" onclick={() => requestVerificationResolution('report', selectedSessionStatus!.session.id, false)} disabled={resolving}>确认未创建</button></div>
        {/if}
          <div class="status-items">{#each selectedSessionStatus.items as item}<article><header><div><strong>费用 #{item.expense_item_id}</strong><small>{item.external_expense_id ? `Concur ID ${item.external_expense_id}` : '尚无外部 ID'} · 尝试 {item.attempt_count} 次</small></div><b class:attention-text={item.status === 'needs_verification'}>{uploadStatusLabel(item.status)}</b></header>{#if item.last_error}<p class="row-error">{item.last_error}</p>{/if}{#if item.status === 'needs_verification'}<div class="resolution"><p>请在 Concur 草稿内核对这条费用是否已经存在。</p><input bind:value={resolutionExternalIds[resolutionKey('expense', item.id)]} placeholder="找到时填写 Concur 费用 ID" /><button type="button" onclick={() => requestVerificationResolution('expense', item.id, true)} disabled={resolving}>确认已创建</button><button type="button" onclick={() => requestVerificationResolution('expense', item.id, false)} disabled={resolving}>确认未创建</button></div>{/if}<ul>{#each item.attachments as attachment}<li><span>材料 #{attachment.document_id}</span><small>{attachment.external_attachment_id ? `Concur ID ${attachment.external_attachment_id}` : `尝试 ${attachment.attempt_count} 次`}</small><b class:attention-text={attachment.status === 'needs_verification'}>{uploadStatusLabel(attachment.status)}</b>{#if attachment.status === 'needs_verification'}<div class="resolution attachment"><p>请在该费用的附件区核对材料是否已上传。</p><input bind:value={resolutionExternalIds[resolutionKey('attachment', attachment.id)]} placeholder="找到时填写 Concur 附件 ID" /><button type="button" onclick={() => requestVerificationResolution('attachment', attachment.id, true)} disabled={resolving}>确认已上传</button><button type="button" onclick={() => requestVerificationResolution('attachment', attachment.id, false)} disabled={resolving}>确认未上传</button></div>{/if}</li>{/each}</ul></article>{/each}</div>
      </section>
    {/if}
  {/if}
</section>

{#if verificationConfirmation}
  <ConfirmDialog title="确认 Concur 中不存在该对象" message="请先在 Concur 中按报销单名称、日期和金额完成核对。只有确认报销单、费用或附件确实未创建后，软件才会解除未知状态并允许安全重试。" confirmLabel="已核对，确认不存在" tone="danger" busy={resolving} onConfirm={() => void resolveVerification(verificationConfirmation!.kind, verificationConfirmation!.objectId, false)} onCancel={() => (verificationConfirmation = null)} />
{/if}

<style>
  .concur-panel{margin-top:1.5rem;border:1px solid #bdb6a8;background:#fbfaf6;color:#17221d}.concur-panel>header{display:flex;justify-content:space-between;gap:1rem;padding:1rem 1.1rem;border-bottom:1px solid #d4cec2;background:#f8f5ed}.eyebrow,.section-title span,.profile-editor>header span,.preflight>header span{color:#6c756f;font-family:'IBM Plex Mono',monospace;font-size:.68rem;font-weight:700;letter-spacing:.07em;text-transform:uppercase}h2,h3{margin:.2rem 0 0}.concur-panel>header p{margin:.3rem 0 0;color:#59645e}.concur-panel>header code{align-self:flex-start;padding:.35rem .45rem;background:#e7e3d9;color:#59645e}.steps{display:grid;grid-template-columns:repeat(4,1fr);border-bottom:1px solid #d4cec2}.steps>div{display:grid;gap:.2rem;padding:.7rem .8rem;border-right:1px solid #d4cec2;background:#efebe2}.steps>div:last-child{border:0}.steps>div.done{background:#e7f1eb}.steps span{font-family:'IBM Plex Mono',monospace;color:#7b837d}.steps small{color:#657068}.message,.state{margin:1rem;padding:.7rem .85rem;border-left:4px solid}.message.notice{border-color:#136b52;background:#e7f1eb;color:#24533f}.message.error{border-color:#b3453e;background:#f8e9e7;color:#862f2a}.state{border-color:#8a928c;background:#efede7}.profile-section,.report-section,.profile-editor,.preflight{margin:1rem;padding:1rem;border:1px solid #d4cec2;background:#fff}.section-title,.profile-editor>header,.preflight>header{display:flex;justify-content:space-between;gap:1rem;align-items:flex-start}.profile-actions{display:flex;gap:.45rem}.profile-actions button,.profile-editor>header button,.gaps button{padding:.35rem .5rem;border:1px solid #8d968f;background:#fff;color:#344139;cursor:pointer}.two,.mapping-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:.6rem}.profile-section label,.profile-editor label,.report-section label{display:grid;gap:.25rem;margin-top:.75rem;color:#47534c;font-size:.74rem;font-weight:700}input,select,textarea{box-sizing:border-box;width:100%;padding:.5rem .55rem;border:1px solid #aeb6af;background:#fff;color:#17221d;font:inherit}textarea{min-height:5rem;resize:vertical}details{margin-top:.75rem;border-top:1px solid #ddd7cb;padding-top:.6rem}summary{cursor:pointer;font-weight:700}.profile-editor details p{color:#657068}.check{display:flex!important;grid-template-columns:auto 1fr!important;align-items:center}.check input{width:auto}.primary{margin-top:1rem;padding:.6rem .8rem;border:1px solid #136b52;background:#136b52;color:#fff;font-weight:700;cursor:pointer}.primary:disabled{opacity:.45;cursor:not-allowed}.counts{display:grid;grid-template-columns:auto auto;gap:.1rem .35rem;text-align:right}.counts b{font-size:1.1rem}.counts small{align-self:center;color:#657068}.gap-summary{display:flex;gap:.5rem;margin-top:.8rem}.gap-summary span{padding:.28rem .45rem;background:#eeeae1;color:#59645e;font-size:.7rem}.gaps{display:grid;gap:.4rem;margin:.75rem 0 0;padding:0;list-style:none}.gaps li{display:grid;grid-template-columns:auto minmax(0,1fr) auto;gap:.65rem;align-items:center;padding:.55rem;border:1px solid #e0dad0}.gaps li>span{padding:.2rem .35rem;background:#fff0d9;color:#805714;font-size:.65rem}.gaps li>span.fact{background:#f8e9e7;color:#862f2a}.gaps li div{display:grid;gap:.12rem}.gaps li strong{font-size:.76rem}.gaps li small{color:#657068}.preflight.ready{border-color:#78a08d}.adapter-gate{margin-top:1rem;padding:.8rem;border-left:4px solid #c47a16;background:#fff4d9;color:#694d18}.adapter-gate p{line-height:1.5}.adapter-gate button{padding:.55rem .7rem;border:1px solid #a7a095;background:#e8e3d9;color:#7b756b}@media(max-width:780px){.steps,.two,.mapping-grid{grid-template-columns:1fr}.profile-actions{display:grid}.gaps li{grid-template-columns:1fr}.concur-panel>header,.section-title,.profile-editor>header,.preflight>header{display:grid}}
  .gaps .override{margin-top:.3rem;font-size:.7rem}.rerun{margin-top:.7rem}
  .projection{margin-top:1rem}.projection>div{display:grid;gap:.55rem;margin-top:.6rem}.projection article{padding:.6rem;border:1px solid #ddd7cb;background:#faf8f2}.projection article>header{display:grid;grid-template-columns:auto 1fr auto;gap:.6rem;align-items:baseline}.projection article>header span{color:#59645e;font-size:.72rem}.projection article>header b{font-size:.7rem}.projection-flow{display:grid;grid-template-columns:minmax(0,1fr) auto minmax(0,1fr);gap:.5rem;align-items:center;margin-top:.5rem}.projection-flow>span{color:#136b52;font-size:.7rem;font-weight:700}.projection pre{max-height:230px;margin:0;padding:.5rem;overflow:auto;background:#efede7;color:#344139;font: .62rem/1.45 'IBM Plex Mono',Consolas,monospace;white-space:pre-wrap}@media(max-width:780px){.projection-flow{grid-template-columns:1fr}.projection-flow>span{text-align:center}}
  .adapter-gate ul{display:grid;gap:.25rem;padding-left:1.15rem;font-size:.72rem}.session-history{margin:1rem;padding:1rem;border:1px solid #d4cec2;background:#fff}.session-history ul{display:grid;gap:.35rem;margin:.65rem 0 0;padding:0;list-style:none}.session-history li{display:grid;grid-template-columns:1fr auto auto auto;gap:.7rem;align-items:center;padding:.5rem;background:#f2efe7;font-size:.72rem}.session-history li.attention{border-left:4px solid #b3453e;background:#f8e9e7}.session-history code{color:#657068}.session-history button,.upload-status button{padding:.35rem .5rem;border:1px solid #8d968f;background:#fff;color:#344139;cursor:pointer}.upload-status{margin:1rem;padding:1rem;border:1px solid #bdb6a8;background:#fff}.upload-status>header,.status-items article>header,.report-row{display:flex;justify-content:space-between;gap:1rem;align-items:flex-start}.upload-status>header>div>span{color:#6c756f;font-family:'IBM Plex Mono',monospace;font-size:.68rem;font-weight:700;letter-spacing:.07em;text-transform:uppercase}.report-row{margin-top:.7rem;padding:.7rem;border-left:4px solid #136b52;background:#e7f1eb}.report-row div,.status-items article>header div{display:grid;gap:.15rem}.report-row small,.status-items small{color:#657068}.status-items{display:grid;gap:.6rem;margin-top:.7rem}.status-items article{padding:.7rem;border:1px solid #ddd7cb;background:#faf8f2}.status-items article>ul{display:grid;gap:.25rem;margin:.55rem 0 0;padding:0;list-style:none}.status-items article>ul>li{display:grid;grid-template-columns:1fr auto auto;gap:.55rem;padding:.45rem;background:#efede7}.attention-text{color:#b3453e}.row-error{margin:.45rem 0 0;color:#862f2a;font-size:.72rem}.resolution{display:grid;grid-template-columns:minmax(0,1fr) auto auto;gap:.4rem;margin-top:.55rem;padding:.55rem;border:1px solid #d7a09b;background:#fff}.resolution p{grid-column:1/-1;margin:0;color:#862f2a;font-size:.72rem}.resolution input{min-width:12rem}.resolution.attachment{grid-column:1/-1}.resolution button:last-child{border-color:#b3453e;color:#862f2a}@media(max-width:780px){.session-history li,.status-items article>ul>li,.resolution{grid-template-columns:1fr}.resolution p{grid-column:auto}}
</style>
