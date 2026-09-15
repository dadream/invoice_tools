import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';

const hash = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
export const isConcurOrigin = value => {
  try { const u = new URL(value); return u.protocol === 'https:' && /(^|\.)(concursolutions\.com|concur\.com|concur\.cn|concurdc\.cn)$/.test(u.hostname); } catch { return false; }
};
export const schemaSignature = fields => hash(fields.map(({ label, required, control }) => ({ label, required, control })).sort((a,b)=>a.label.localeCompare(b.label)));
export function suggestedSource(label, scope) {
  const rules = scope === 'report' ? [
    ['report_name', /费用报告名称|报销单名称|Report Name/i], ['report_date', /费用报告日期|Report Date/i], ['comment', /^Comment$|备注/i],
  ] : [
    ['transaction_date', /交易日期|Transaction Date/i], ['gross_amount', /^金额|^Amount$/i],
    ['description', /商务用途|Business Purpose/i], ['counterparty_name', /供应商|Vendor/i],
    ['city_name', /City of Purchase|消费城市/i], ['province_name', /州\/省|State|Province/i],
    ['payment_method', /付款类型|Payment Type/i], ['currency_code', /^币种|^Currency/i], ['comment', /^Comment$|备注/i],
  ];
  // VAT and company-specific policy fields deliberately require explicit confirmation.
  return rules.find(([,pattern])=>pattern.test(label))?.[0] ?? '';
}

export async function readForm(page, scope) {
  await page.locator('input:not([type=hidden]):not([type=password]):not([type=file]),textarea,select,[role=combobox]').filter({visible:true}).first().waitFor({state:'visible',timeout:10000});
  const fields = await page.locator('input:not([type=hidden]):not([type=password]):not([type=file]), textarea, select, [role=combobox], [role=checkbox]').evaluateAll(nodes => {
    const seen = new Set();
    return nodes.filter(e => e.getClientRects().length && !e.disabled && e.getAttribute('aria-disabled') !== 'true' && !e.readOnly && !['submit','button','reset'].includes(e.type)).flatMap(e => {
      const label = (e.labels?.[0]?.textContent || e.getAttribute('aria-label') || (e.getAttribute('aria-labelledby') || '').split(' ').map(id=>document.getElementById(id)?.textContent || '').join(' ')).replace(/\s+/g,' ').trim();
      if (!label || seen.has(label)) return [];
      seen.add(label);
      return [{ label, required: e.required || e.getAttribute('aria-required') === 'true' || /[*＊]/.test(label), control: e.tagName === 'SELECT' ? 'select' : e.type === 'checkbox' || e.getAttribute('role') === 'checkbox' ? 'checkbox' : e.getAttribute('role') === 'combobox' ? 'combobox' : e.type === 'date' ? 'date' : 'text', options: e.tagName === 'SELECT' ? Array.from(e.options).filter(o=>!o.disabled).map(o=>o.textContent.trim()).filter(Boolean) : [] }];
    });
  });
  if (!fields.length) throw new Error('未找到可填写表单。请在浏览器打开新建报销单或对应类型的费用详情，再读取表单。');
  return { signature: schemaSignature(fields), fields: fields.map(f=>({...f,source:suggestedSource(f.label,scope),constant:'',option_map:{},format:''})) };
}

async function unique(locator, message) {
  const visible = locator.filter({ visible: true });
  if (await visible.count() !== 1) throw new Error(message);
  return visible;
}
async function click(page, labels) {
  const locator = page.getByRole('button', { name: new RegExp(`^(?:${labels.join('|')})$`, 'i') });
  await (await unique(locator, `无法唯一定位“${labels[0]}”。请打开对应页面后继续。`)).click();
}
async function fillForm(page, step) {
  const form = await readForm(page, step.kind);
  if (form.signature !== step.signature) throw new Error('当前表单与保存的映射不同（可能新增必填字段或费用明细）。请重新读取表单并确认映射。');
  for (const f of step.values) {
    const locator = await unique(page.getByLabel(f.label, { exact: true }), `字段“${f.label}”无法唯一定位，请更新映射。`);
    let value = f.value;
    if (f.control === 'date') value = value.slice(0,10);
    if (f.control === 'select') await locator.selectOption({ label: value });
    else if (f.control === 'checkbox') {
      if (!['true','false'].includes(value)) throw new Error(`“${f.label}”需要明确的是/否值。`);
      await locator.setChecked(value === 'true');
    } else if (f.control === 'combobox') {
      await locator.fill(value);
      await (await unique(page.getByRole('option', { name: value, exact: true }), `“${f.label}”未找到唯一选项：${value}`)).click();
    } else await locator.fill(value);
  }
}

async function readValues(page, step) {
  for (const f of step.values) {
    const locator = await unique(page.getByLabel(f.label,{exact:true}), `保存后未能回读“${f.label}”。请打开该费用详情，再点击核对。`);
    const actual = f.control === 'select' ? await locator.locator('option:checked').innerText() : f.control === 'checkbox' ? String(await locator.isChecked()) : await locator.inputValue();
    const normalize = s=>s.trim().replace(/\r\n/g,'\n');
    const decimal=s=>{s=s.replaceAll(',','').trim();if(!/^[-+]?\d+(\.\d+)?$/.test(s))return null;let [a,b='']=s.replace(/^\+/,'').split('.');a=a.replace(/^(-?)0+(?=\d)/,'$1');b=b.replace(/0+$/,'');return a+(b?'.'+b:'');};
    const equivalent=['gross_amount','tax_amount'].includes(f.source)?decimal(actual)!==null&&decimal(actual)===decimal(f.value):normalize(actual)===normalize(f.value);
    if (!equivalent) throw new Error(`“${f.label}”回读与填写值不同，未判定通过。请在 Concur 核对。`);
  }
}

export class ConcurAdapter {
  constructor(context) { this.context = context; this.identity = null; this.reportListUrl=null; }
  async page() {
    const pages = this.context.pages().filter(p=>!p.isClosed() && isConcurOrigin(p.url()));
    if (pages.length !== 1) throw new Error('请在专用浏览器仅保留一个 Concur 页面，并完成登录和账号选择。');
    const top = pages[0];
    const frames = top.frames().filter(f=>isConcurOrigin(f.url()));
    // Concur may render its app in a same-origin frame. Choose the only visible form host.
    for (const f of frames.slice().reverse()) if (await f.locator('input:not([type=hidden]),table,[role=table]').count()) return {top,page:f};
    return {top,page:top};
  }
  async status() {
    const {top,page} = await this.page();
    const body = await page.locator('body').innerText();
    const candidates = [...new Set((await page.locator('header,[role=banner],[aria-label*=账户],[aria-label*=账号],[aria-label*=Profile],[role=menu]').filter({visible:true}).allInnerTexts()).join(' ').match(/[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/gi) || [])];
    if (/输入密码|Enter your password|Sign in to your account/i.test(body)) this.identity = null;
    const reports_read=/报销单|Expense Reports|Manage Expenses/i.test(body) && /创建报销单|Create Report|Create New Report/i.test(body) && await page.locator('table,[role=table],[role=grid]').count()>0;
    if(reports_read)this.reportListUrl=top.url();
    return {origin:new URL(top.url()).origin, accounts:candidates, connected:!!this.identity, account:this.identity?.account || '', reports_read};
  }
  async confirm(account) {
    const status = await this.status();
    if (!status.reports_read || !status.accounts.includes(account)) throw new Error('请进入报销单管理页，打开个人资料/账号菜单，再确认实际登录的账号。');
    this.identity = {account,origin:status.origin};
    return this.identity;
  }
  async guard(profile) {
    let status = await this.status();
    if (!status.accounts.length) {
      const {page}=await this.page();
      const menu=page.getByRole('button',{name:/个人资料|个人信息|账号|账户|用户菜单|profile|account menu/i}).filter({visible:true});
      if(await menu.count()===1){
        await menu.click();
        try { await page.getByText(profile.account,{exact:false}).filter({visible:true}).first().waitFor({timeout:3000}); } catch {}
        status=await this.status();
        await page.locator('body').press('Escape');
      }
    }
    if (!this.identity || profile.account !== this.identity.account || profile.origin !== status.origin || status.accounts.length!==1 || status.accounts[0]!==profile.account) throw new Error('当前账号无法唯一确认或已变化。请打开个人资料菜单重新确认账号，不能继续旧计划。');
    return this.page();
  }
  async discover(profile,scope) { const {page}=await this.guard(profile); return readForm(page,scope); }
  async verify(job, index) {
    const step=job.steps[index]; const {top,page}=await this.guard(job.profile);
    // A verification must survive reload; values left in an unsaved form do not count.
    await top.reload({waitUntil:'domcontentloaded'});
    const current=(await this.guard(job.profile)).page;
    const body=await current.locator('body').innerText();
    if (step.kind==='report') {
      if (!body.includes(step.label) || !/未提交|Not Submitted|Draft/i.test(body)) throw new Error('尚未回读到指定未提交草稿。请打开该草稿后核对。');
    } else if (step.kind==='expense') {
      if (!body.includes(step.report_key)) throw new Error('不能确认费用属于本次报销单。请打开本次报销单内该费用后核对。');
      await readValues(current,step);
    } else {
      const expense=job.steps.find(s=>s.key===step.expense_key);
      await readValues(current,expense);
      const required=job.steps.filter((s,i)=>s.kind==='attachment' && s.expense_key===step.expense_key && (s.status==='verified'||i===index));
      const attachments=await current.getByRole('link').allTextContents();
      // Must prove every previously appended file still exists, never just the newest name.
      for(const item of required) if(!attachments.some(t=>t.trim()===item.document.name)) throw new Error(`未回读到附件“${item.document.name}”。请打开附件清单后核对；若被替换，本租户不支持当前逐份追加方式。`);
    }
    return {url:top.url(),message:'已保存并刷新回读通过'};
  }
  async execute(job,index) {
    const step=job.steps[index]; const {top}=await this.guard(job.profile);
    if(step.kind==='report') {
      if(!this.reportListUrl)throw new Error('请先打开报销单管理页并点击读取账号及报销单。');
      await top.goto(this.reportListUrl,{waitUntil:'domcontentloaded'});
      const {page}=await this.page();
      if((await page.locator('body').innerText()).includes(step.label)) throw new Error('页面已有同名草稿，请先核对，不重复创建。');
      const nameField=step.values.find(v=>v.source==='report_name')??step.values[0];
      // The user may have left the create dialog open during schema discovery.
      // Never click its submit button before filling the intended frozen report name.
      if(!nameField || await page.getByLabel(nameField.label,{exact:true}).filter({visible:true}).count()!==1) await click(page,['创建报销单','Create Report','Create New Report']);
      await fillForm(page,step);
      await click(page,['创建报销单','Create Report','Create New Report']);
    } else {
      const owner=job.steps.find(s=>s.key===(step.kind==='expense'?step.report_key:step.expense_key));
      if(!owner?.remote_url || !isConcurOrigin(owner.remote_url) || new URL(owner.remote_url).origin!==job.profile.origin) throw new Error('缺少已验证的目标链接，请先核对上一步。');
      await top.goto(owner.remote_url,{waitUntil:'domcontentloaded'});
      const {page}=await this.guard(job.profile);
      if(step.kind==='expense') {
        await click(page,['添加费用','新建费用','Add Expense','New Expense','手动创建费用','Create New Expense']);
        const manual=page.getByRole('tab',{name:/^手动创建费用$|^Create New Expense$/i});
        if(await manual.count()===1) await manual.click();
        await (await unique(page.getByText(job.profile.expense_types[step.category],{exact:true}),'找不到唯一费用类型，请更新目标类型映射。')).click();
        await fillForm(page,step);
        await click(page,['保存费用','保存','Save Expense','Save']);
      } else {
        await readValues(page,owner);
        const previous=job.steps.filter(s=>s.kind==='attachment'&&s.expense_key===step.expense_key&&s.status==='verified');
        const names=await page.getByRole('link').allTextContents();
        if(names.some(n=>n.trim()===step.document.name)) throw new Error('该文件名已存在，请先核对上传结果。');
        for(const p of previous) if(!names.some(n=>n.trim()===p.document.name)) throw new Error('之前的附件未显示，暂停追加以防覆盖，请打开完整附件清单。');
        const extension=step.document.name.split('.').at(-1).toLowerCase();
        const mimeType=({pdf:'application/pdf',jpg:'image/jpeg',jpeg:'image/jpeg',png:'image/png',tif:'image/tiff',tiff:'image/tiff'})[extension];
        if(!mimeType)throw new Error('不支持该材料格式，请先准备独立 PDF 或图片。');
        // Storage paths can be hash-based. Upload the original display filename, not that hash.
        const filePayload={name:step.document.name,mimeType,buffer:await readFile(step.document.path)};
        const input=page.locator('input[type=file]');
        if(await input.count()!==1) {
          const chooser=top.waitForEvent('filechooser',{timeout:10000});
          await click(page,['添加','上传发票','Attach Receipt','Upload Receipt','Add Receipt']);
          const file=await chooser; await file.setFiles(filePayload);
        } else await input.setInputFiles(filePayload);
        // Some tenants upload immediately; others require an explicit upload/save button.
        const upload=page.getByRole('button',{name:/^(上传|Upload|Attach)$/i}).filter({visible:true});
        if(await upload.count()===1) await upload.click();
      }
    }
    // Do not reload while a save/upload is in flight. A non-idle page is an uncertain write.
    await top.waitForLoadState('networkidle',{timeout:15000});
    // UI save/processing can be asynchronous. Poll read-only verification, never repeat writes.
    let error;
    for(let n=0;n<3;n++) {
      try { return await this.verify(job,index); } catch(e) { error=e; if(n<2) await new Promise(r=>setTimeout(r,1500)); }
    }
    throw error;
  }
}
