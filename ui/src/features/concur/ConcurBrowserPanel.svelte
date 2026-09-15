<script lang="ts">
  import {onMount} from 'svelte'
  import {invokeSafe,describeError} from '../../lib/ipc'
  import FormMapping from './FormMapping.svelte'
  import {categories,stepName,completed,progress,type Workspace,type Form,type Job} from './types'
  let {batchId=null,onComplete=()=>{}}:{batchId?:number|null;onComplete?:()=>void}=$props()
  let stage=$state(1)
  let workspace=$state<Workspace|null>(null),error=$state(''),notice=$state(''),busy=$state(false)
  let portal=$state('https://www.concursolutions.com/'),accounts=$state<string[]>([]),selectedAccount=$state(''),connected=$state(false),readStatus=$state('未测试')
  let scope=$state('report'),job=$state<Job|null>(null),authorized=$state(false),running=$state(false),stop=$state(false)
  let mappingConfirmed=$state(false),retryAbsent=$state(false)
  const testMode=$derived(batchId===null)
  const expenseCategories=$derived(workspace?[...new Set(workspace.snapshot.expenses.map(e=>e.fields.category_code))]:[])
  const unknownGroups=$derived(workspace?[...new Map(workspace.snapshot.expenses.filter(e=>!['local_month','business_trip'].includes(e.group_kind)).map(e=>[e.group_id,e])).values()]:[])
  const sourceValues=$derived.by(()=>{const result:Record<string,string[]>={};for(const expense of workspace?.snapshot.expenses??[]){if(scope==='report'||expense.fields.category_code!==scope)continue;for(const [key,value]of Object.entries(expense.fields)){if(value){result[key]??=[];if(!result[key].includes(value))result[key].push(value)}}}return result})
  const currentForm=$derived(workspace?(scope==='report'?workspace.profile.report:workspace.profile.expenses[scope]):null)
  const next=$derived(job?.steps.find(s=>s.status!=='verified'))
  async function call<T>(command:string,args:Record<string,unknown>={}):Promise<T|null>{const result=await invokeSafe<T>(command,args);if(!result.ok){error=describeError(result.error);return null}return result.data}
  async function load(){busy=true;error='';workspace=await call<Workspace>('get_concur_browser_workspace',{batchId});busy=false;if(workspace?.jobs.length)job=workspace.jobs[0]}
  async function control(action:string,payload:Record<string,unknown>={}){busy=true;error='';const data=await call<Record<string,unknown>>('concur_browser_control',{action,payload});busy=false;return data}
  async function launch(){const result=await control('launch',{url:portal});if(result){connected=false;readStatus='未测试';notice='请在新打开的 Edge 中登录、选择公司账号，然后打开个人资料/账号菜单，再读取账号。'}}
  async function readAccount(){const data=await control('status');readStatus=data?(data.reports_read===true?'通过':'需要用户处理'):'失败';if(data){accounts=data.accounts as string[];selectedAccount=accounts[0]??'';notice=accounts.length?'请选择实际操作的账号并确认。':'请在 Concur 打开个人资料/账号菜单，让当前账号邮箱可见，再读取。'}}
  async function confirmAccount(){const data=await control('confirm',{account:selectedAccount});if(data&&workspace){workspace.profile.account=String(data.account);workspace.profile.origin=String(data.origin);connected=true;mappingConfirmed=false;stage=2;notice='账号已确认。已有映射可确认后继续使用；仅首次配置或表单变化时需要重新读取。'}}
  async function discover(){if(!workspace)return;const form=await control('discover',{profile:workspace.profile,scope:scope==='report'?'report':'expense'});if(form){if(scope==='report')workspace.profile.report=form as unknown as Form;else workspace.profile.expenses[scope]=form as unknown as Form;mappingConfirmed=false;notice='已读取表单。候选字段仍需确认；VAT、公司专用字段不会自动猜测。'}}
  async function saveMapping(){if(!workspace)return;busy=true;error='';const previousVersion=workspace.profile.version;const profile=await call<Workspace['profile']>('save_concur_browser_profile',{profile:workspace.profile});busy=false;if(profile){workspace.profile=profile;stage=3;notice=profile.version===previousVersion?'映射未变化，已复用原版本；已有通过的测试仍然有效。':'映射已保存。相同账号和映射内容的已通过测试可复用；内容变化需重新测试。冻结计划保持不变。'}}
  async function prepare(save:boolean){if(!workspace)return;busy=true;error='';const result=await call<Job>('prepare_concur_browser_job',{batchId,profile:workspace.profile,save});busy=false;if(result){job=result;if(save){workspace.jobs=[result,...workspace.jobs];stage=4;notice='计划已冻结。点击开始后将创建未提交草稿。'}else notice=result.gaps.length?'请补齐下面列出的缺口。':'预检通过。确认授权后冻结计划。'}}
  const persisted=$derived(!!job&&!!workspace?.jobs.some(j=>j.id===job?.id))
  async function execute(operation='execute'){
    if(!job)return;busy=true;error='';const updated=await call<Job>('run_concur_browser_step',{jobId:job.id,operation});busy=false;
    if(updated){job=updated;if(workspace)workspace.jobs=workspace.jobs.map(j=>j.id===updated.id?updated:j);if(completed(updated))onComplete()}
    retryAbsent=false;return updated
  }
  async function start(){if(!authorized||!persisted)return;running=true;stop=false;try{while(!stop){const updated=await execute();if(!updated||completed(updated)||updated.steps.find(s=>s.status!=='verified')?.status==='writing')break}}finally{running=false}}
  onMount(()=>{void load()})
</script>

<section class="concur-browser">
  <header><h2>{testMode?'Concur 填报测试':'上传到 Concur'}</h2><p>{testMode?'使用软件包内的独立示例费用和 PDF，验证当前公司账号是否支持填报。不会读取真实批次。':'按本地费用、差旅费用分别创建草稿；逐条填写费用，逐份追加材料。'}</p></header>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if notice}<p class="notice" role="status">{notice}</p>{/if}
  {#if !workspace}<p>{busy?'正在读取…':'无法读取配置或内置样例。'} <button disabled={busy} onclick={load}>重新读取</button></p>
  {:else}
    <div class="identity"><strong>已连接账号：{connected?workspace.profile.account:'尚未确认'}</strong><span>{connected?workspace.profile.origin:'不需要 Client ID 或密钥；登录和 MFA 由用户完成。'}</span></div>
    <nav class="toolbar" aria-label="Concur 填报步骤">{#each ['连接浏览器','字段映射',testMode?'测试实例':'交付计划','执行结果'] as label,index}<button class:primary={stage===index+1} onclick={()=>stage=index+1}>{index+1}. {label}</button>{/each}</nav>
    <section class="block" hidden={stage!==1}><h3>1. 连接浏览器</h3>
      <div class="toolbar"><label>Concur 门户地址<input aria-label="Concur 门户地址" bind:value={portal} /></label><button disabled={busy||running} onclick={launch}>打开 Edge 并登录</button><button disabled={busy||running} onclick={readAccount}>读取账号及报销单</button></div>
      {#if accounts.length}<div class="toolbar"><select aria-label="当前 Concur 账号" bind:value={selectedAccount}>{#each accounts as account}<option value={account}>{account}</option>{/each}</select><button disabled={busy||running} onclick={confirmAccount}>确认此账号</button></div>{/if}
      <p class="hint">报销单读取：{readStatus}。专用浏览器不复制日常浏览器 Cookie；关闭软件后需重新登录。企业限制浏览器控制时，此方式可能不可用。</p>
    </section>
    <section class="block" hidden={stage!==2}><h3>2. 读取并保存字段映射</h3>
      <p class="hint">首次配置：在浏览器打开“创建报销单”表单，读取后取消；再打开各类费用的新建表单，逐类读取。这里只读取字段，不保存 Concur 数据。</p>
      <div class="toolbar"><select aria-label="读取的表单" bind:value={scope}><option value="report">报销单表单</option>{#each expenseCategories as category}<option value={category}>{categories[category]??category}费用表单</option>{/each}</select>
      {#if scope!=='report'}<label>Concur 完整费用类型名称<input bind:value={workspace.profile.expense_types[scope]} placeholder="按浏览器中实际选项填写" /></label>{/if}
      <button disabled={!connected||busy||running} onclick={discover}>读取当前表单</button></div>
      {#if currentForm?.fields.length}<FormMapping form={currentForm} scope={scope==='report'?'report':'expense'} values={sourceValues}/>{/if}
      <div class="toolbar"><label class="check"><input type="checkbox" bind:checked={mappingConfirmed}/>我已确认字段含义和选项，金额使用票面实际金额</label><button disabled={!mappingConfirmed||busy||running||!connected} onclick={saveMapping}>保存映射</button></div>
    </section>
    <section class="block" hidden={stage!==3}><h3>3. {testMode?'内置测试实例':'交付计划'}</h3>
      <p class="hint">{testMode?'示例为虚构数据，所有 PDF 均带测试标识。会在真实 Concur 账号创建测试草稿，绝不能提交报销。':'所有本地组汇总为一张草稿，所有差旅组汇总为另一张草稿。未计入和疑似重复费用不上传。'}</p>
      <table><thead><tr><th>分组</th><th>费用</th><th>实际金额</th><th>逐份上传的文件</th></tr></thead><tbody>{#each workspace.snapshot.expenses as expense}<tr><td>{expense.group_title}</td><td>{categories[expense.fields.category_code]??expense.fields.category_code} · {expense.fields.transaction_date}</td><td>{expense.fields.currency_code} {expense.fields.gross_amount}</td><td>{expense.documents.map(d=>d.name).join('、')}</td></tr>{/each}</tbody></table>
      {#each unknownGroups as group}<label class="toolbar">{group.group_title}的目标报销单<select bind:value={workspace.profile.group_targets[String(group.group_id)]}><option value="">请选择</option><option value="local">本地费用</option><option value="trip">差旅费用</option></select></label>{/each}
      <div class="toolbar"><button disabled={busy||running} onclick={()=>prepare(false)}>检查映射及材料</button><label class="check"><input type="checkbox" bind:checked={authorized}/>{testMode?'我授权使用示例数据创建测试草稿，并在测试后自行删除':'我授权创建未提交草稿，金额不应用企业报销上限'}</label><button class="primary" disabled={!authorized||!mappingConfirmed||busy||running||!connected} onclick={()=>prepare(true)}>冻结{testMode?'测试':'交付'}计划</button></div>
      {#if job?.gaps.length}<div class="error"><strong>需要补齐（{job.gaps.length}）</strong><ul>{#each job.gaps as gap}<li>{gap}</li>{/each}</ul></div>{/if}
    </section>
    {#if job&&persisted}
      <section class="block" hidden={stage!==4}><div class="toolbar"><h3>4. 执行结果 {progress(job).done} / {progress(job).total}</h3><select aria-label="交付记录" disabled={running||busy} value={job.id} onchange={(e)=>{job=workspace?.jobs.find(j=>j.id===e.currentTarget.value)??null;authorized=false}}>{#each workspace.jobs as entry,index}<option value={entry.id}>{entry.test_mode?'测试':'交付'} {workspace.jobs.length-index} · {completed(entry)?'已完成':'待继续'}</option>{/each}</select></div>
        <progress max={progress(job).total} value={progress(job).done}></progress>
        <table><thead><tr><th>能力/步骤</th><th>对象</th><th>结果</th></tr></thead><tbody>{#each job.steps as step}<tr><td>{stepName[step.kind]}</td><td>{step.label}</td><td><strong>{step.status==='verified'?'通过':step.status==='writing'?'需要用户处理':'未测试'}</strong>{#if step.message}<small>{step.message}</small>{/if}</td></tr>{/each}</tbody></table>
        {#if completed(job)}<p class="notice">{testMode?'样例各步骤保存及回读通过。请在 Concur 检查两张测试草稿、费用和每份附件，测试后自行删除草稿。真实批次仍需使用同一账号及映射。':'草稿创建完成。请在 Concur 最终复核并自行提交。'}</p>
        {:else if next?.status==='writing'}<p class="hint">该步骤可能已经保存。请在浏览器打开相应草稿/费用或附件清单，先核对，不能直接重建。</p><div class="toolbar"><button disabled={busy||running||!connected} onclick={()=>execute('verify')}>刷新回读并核对本页</button><label class="check"><input type="checkbox" bind:checked={retryAbsent}/>我已在 Concur 确认该步骤尚未保存</label><button disabled={!retryAbsent||busy||running||!connected} onclick={()=>execute('retry_confirmed_absent')}>允许重试此步骤</button></div>
        {:else}<div class="toolbar"><button class="primary" disabled={!authorized||busy||running||!connected} onclick={start}>{running?'正在执行…':'开始 / 继续填写草稿'}</button>{#if running}<button onclick={()=>{stop=true;notice='当前步骤结束后暂停，已完成进度保留。'}}>暂停</button>{/if}</div>{/if}
      </section>
    {:else if stage===4}<p class="hint">尚无冻结计划，请先检查映射和材料，确认后冻结计划。</p>{/if}
  {/if}
</section>
<style>
.concur-browser{color:#17232d;max-width:1500px}header h2{margin:0;font-size:1.5rem}header p,.hint{color:#596870;line-height:1.6;font-size:.88rem}.identity{display:flex;justify-content:space-between;gap:1rem;padding:.75rem 1rem;background:#eaf3ef;border-left:4px solid #136b52;flex-wrap:wrap}.identity span{color:#596870;font-size:.85rem}.block{margin-top:1rem;background:#fff;border:1px solid #cbd2d6;padding:1rem;min-width:0}.block h3{margin:0 0 .65rem;font-size:1.05rem}.toolbar{display:flex;gap:.7rem;align-items:center;flex-wrap:wrap;margin:.75rem 0}.toolbar label:not(.check){display:grid;gap:.3rem;flex:1;min-width:200px}.check{display:flex;align-items:center;gap:.5rem;color:#596870;font-size:.85rem}.check input{min-height:auto;width:auto}input,select,button{box-sizing:border-box;min-height:38px;padding:.5rem .7rem;border:1px solid #aebcc1;background:#fff;color:#17232d}input{min-width:0}button{color:#136b52;font-weight:700;cursor:pointer}.primary{background:#136b52;color:white;border-color:#136b52}button:disabled{opacity:.5;cursor:not-allowed}.error{background:#fff2ed;color:#8f351a;padding:.8rem;overflow-wrap:anywhere}.notice{background:#eaf3ef;color:#155c47;padding:.8rem}table{width:100%;border-collapse:collapse;table-layout:fixed}th,td{text-align:left;padding:.65rem .75rem;border-bottom:1px solid #d9dfe2;overflow-wrap:anywhere;vertical-align:top}th{background:#f1f3f4;color:#596870;font-size:.74rem}td{font-size:.85rem}small{display:block;color:#657078;margin-top:.3rem;line-height:1.5}progress{width:100%;accent-color:#136b52}li{margin:.4rem 0}
</style>
