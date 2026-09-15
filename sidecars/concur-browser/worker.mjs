// Private stdio protocol. No HTTP server, browser cookies, password or token export.
import { chromium } from 'playwright-core';
import { createInterface } from 'node:readline';
import { ConcurAdapter } from './adapter.mjs';
let browser,adapter;
const lines=createInterface({input:process.stdin,crlfDelay:Infinity});
async function request({op,args={}}) {
  if(op==='launch') {
    const url=new URL(args.url);
    if(url.protocol!=='https:' || url.username || url.password) throw new Error('门户地址必须是无账号密码的 HTTPS 地址。');
    if(browser) await browser.close();
    browser=await chromium.launch({channel:'msedge',headless:false,args:['--start-maximized']});
    const context=await browser.newContext({viewport:null,acceptDownloads:false});
    const page=await context.newPage(); await page.goto(url.href,{waitUntil:'domcontentloaded'});
    adapter=new ConcurAdapter(context);
    return {message:'浏览器已打开，请由用户完成登录和账号选择。'};
  }
  if(op==='close') { await browser?.close(); browser=null; adapter=null; return {}; }
  if(!adapter) throw new Error('请先打开专用浏览器并登录 Concur。');
  if(op==='status') return adapter.status();
  if(op==='confirm') return adapter.confirm(args.account);
  if(op==='discover') return adapter.discover(args.profile,args.scope);
  if(op==='execute') return adapter.execute(args.job,args.index);
  if(op==='verify') return adapter.verify(args.job,args.index);
  throw new Error('不支持的浏览器操作。');
}
for await(const line of lines) {
  try { const data=await request(JSON.parse(line)); process.stdout.write(JSON.stringify({ok:true,data})+'\n'); }
  catch(error) {
    // Playwright error call logs can include field values/URLs. Only our plain business errors escape.
    const message=String(error?.message||'');
    const safe=/[\u4e00-\u9fff]/.test(message)&&!message.includes('Call log:') ? message.slice(0,500) : '浏览器操作未完成。请检查登录、网络和当前页面，再回读核对；不要重复创建。';
    process.stdout.write(JSON.stringify({ok:false,error:safe})+'\n');
  }
}
await browser?.close();
