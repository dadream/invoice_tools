import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {fileURLToPath} from 'node:url';
import {chromium} from 'playwright-core';
import {ConcurAdapter,isConcurOrigin,schemaSignature,suggestedSource,readForm} from '../adapter.mjs';

test('allows only HTTPS Concur domains and does not guess VAT or company limits',()=>{
  assert.equal(isConcurOrigin('https://www.concursolutions.com'),true);
  for(const url of ['http://www.concursolutions.com','https://concursolutions.com.evil.example','file:///x','https://evilconcur.com'])assert.equal(isConcurOrigin(url),false);
  assert.equal(suggestedSource('VAT 金额 *','expense'),'');
  assert.equal(suggestedSource('金额 *','expense'),'gross_amount');
  assert.equal(suggestedSource('核准金额','expense'),'');
  assert.equal(schemaSignature([{label:'A',required:true,control:'text'}]),schemaSignature([{label:'A',required:true,control:'text',source:'other'}]));
});

test('isolated browser fixture: 2 drafts, 3 expenses, 6 appended files, reload checks and overwrite rejection',async()=>{
  // All requests are intercepted. No real portal, user data, or external write is accessed.
  const browser=await chromium.launch({channel:'msedge',headless:true});
  try {
    const context=await browser.newContext();
    let account='synthetic@example.invalid'; const reports=[],expenses=[];let overwrite=false;
    const esc=v=>String(v??'').replaceAll('&','&amp;').replaceAll('"','&quot;').replaceAll('<','&lt;');
    const input=(label,value='')=>`<label>${label}<input name="${label}" value="${esc(value)}" required></label>`;
    const reportFields=['费用报告名称','费用报告日期'];
    const expenseFields=['交易日期','金额','Comment'];
    await context.route('**/*',async route=>{
      const request=route.request(); const url=new URL(request.url());
      if(url.origin!=='https://test.concursolutions.com')return route.abort();
      const params=url.pathname.split('/'); let body='';
      if(request.method()==='POST'){
        const value=JSON.parse(request.postData());
        if(params[1]==='reports'){reports.push(value);body=`/report/${reports.length-1}`;}
        if(params[1]==='expenses'){expenses.push({...value,files:[]});body=`/expense/${expenses.length-1}`;}
        if(params[1]==='upload'){if(overwrite)expenses[value.id].files=[];expenses[value.id].files.push(value.name);body='ok';}
        return route.fulfill({contentType:'text/plain',body});
      }
      if(url.pathname==='/manage')body='<h1>报销单</h1><table><tr><td>现有草稿</td></tr></table><button onclick="location.href=\'/create\'">创建报销单</button>';
      if(url.pathname==='/create')body=`<form>${reportFields.map(f=>input(f)).join('')}<button>创建报销单</button></form><script>document.querySelector('form').onsubmit=async e=>{e.preventDefault();location.href=await (await fetch('/reports',{method:'POST',body:JSON.stringify(Object.fromEntries(new FormData(e.target)))})).text()}</script>`;
      if(params[1]==='report'){const r=reports[+params[2]];body=`<h1>${esc(r['费用报告名称'])}</h1><p>未提交</p><button onclick="location.href='/types/${params[2]}'">添加费用</button>`;}
      if(params[1]==='types')body=['rail','hotel','city_transport'].map(c=>`<button onclick="location.href='/new/${params[2]}/${c}'">测试-${c}</button>`).join('');
      if(params[1]==='new')body=`<h1>${esc(reports[+params[2]]['费用报告名称'])}</h1><form>${expenseFields.map(f=>input(f)).join('')}<button>保存费用</button></form><script>document.querySelector('form').onsubmit=async e=>{e.preventDefault();location.href=await(await fetch('/expenses',{method:'POST',body:JSON.stringify({...Object.fromEntries(new FormData(e.target)),report:${params[2]}})})).text()}</script>`;
      if(params[1]==='expense'){const e=expenses[+params[2]];body=`<h1>${esc(reports[e.report]['费用报告名称'])}</h1><p>未提交</p>${expenseFields.map(f=>input(f,e[f])).join('')}${e.files.map(name=>`<a href='/file'>${esc(name)}</a>`).join('')}<input type=file onchange="fetch('/upload',{method:'POST',body:JSON.stringify({id:${params[2]},name:this.files[0].name})}).then(()=>location.reload())">`;}
      return route.fulfill({contentType:'text/html',body:`<!doctype html><html><head><meta charset="utf-8"></head><body><header>${account}</header>${body}</body></html>`});
    });
    const page=await context.newPage();const adapter=new ConcurAdapter(context);
    await page.goto('https://test.concursolutions.com/manage');
    assert.equal((await adapter.status()).reports_read,true);
    const identity=await adapter.confirm(account);
    await page.goto('https://test.concursolutions.com/create');
    const reportForm=await readForm(page,'report');
    const expenseSignature=schemaSignature(expenseFields.map(label=>({label,required:true,control:'text'})));
    const kit=JSON.parse(fs.readFileSync(new URL('../samples/sample.json',import.meta.url),'utf8'));
    const job={profile:{...identity,expense_types:{rail:'测试-rail',hotel:'测试-hotel',city_transport:'测试-city_transport'}},steps:[]};
    for(const target of ['local_month','business_trip']){
      const rkey=`IA-test-${target}`,label=`[测试-请勿提交] ${target} [${rkey}]`;
      job.steps.push({key:rkey,kind:'report',label,report_key:rkey,status:'pending',signature:reportForm.signature,values:[{label:'费用报告名称',value:label,control:'text'},{label:'费用报告日期',value:'2026-06-30',control:'text'}]});
      for(const expense of kit.expenses.filter(e=>e.group_kind===target)){
        const ekey=`${rkey}-E${expense.id}`;
        job.steps.push({key:ekey,kind:'expense',report_key:rkey,expense_key:ekey,category:expense.fields.category_code,status:'pending',signature:expenseSignature,values:[{label:'交易日期',value:expense.fields.transaction_date,control:'text'},{label:'金额',value:expense.fields.gross_amount,control:'text'},{label:'Comment',value:ekey,control:'text'}]});
        for(const doc of expense.documents)job.steps.push({key:`${ekey}-D${doc.id}`,kind:'attachment',report_key:rkey,expense_key:ekey,status:'pending',document:{...doc,path:fileURLToPath(new URL(`../samples/${doc.path}`,import.meta.url))}});
      }
    }
    for(let i=0;i<job.steps.length;i++){
      const s=job.steps[i];if(s.kind==='report')await page.goto('https://test.concursolutions.com/manage');
      const result=await adapter.execute(job,i);s.status='verified';s.remote_url=result.url;
    }
    assert.equal(reports.length,2);assert.equal(expenses.length,3);assert.deepEqual(expenses.map(e=>e.files.length),[1,2,3]);
    assert.equal(expenses.reduce((sum,e)=>sum+Math.round(Number(e['金额'])*100),0),666);
    // Re-read after reconnecting is read-only, no duplicate create/upload.
    await adapter.verify(job,job.steps.length-1);assert.equal(reports.length,2);assert.equal(expenses.length,3);
    overwrite=true;
    const last=job.steps.at(-1);const extra={...last,key:'test-extra',status:'pending',document:{...last.document,name:'local-invoice.pdf',path:fileURLToPath(new URL('../samples/local-invoice.pdf',import.meta.url))}};
    job.steps.push(extra);
    await assert.rejects(()=>adapter.execute(job,job.steps.length-1),/未回读到附件/);
    // Account switch may never reuse the previous confirmed identity.
    account='different@example.invalid';await page.reload();await assert.rejects(()=>adapter.guard(job.profile),/账号/);
    account='';await page.reload();await assert.rejects(()=>adapter.guard(job.profile),/账号/);
  }finally{await browser.close();}
});
