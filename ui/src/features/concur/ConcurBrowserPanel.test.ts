// @vitest-environment jsdom
import {mount,tick,unmount} from 'svelte'
import {afterEach,beforeEach,describe,it,expect,vi} from 'vitest'
import {invokeSafe} from '../../lib/ipc'
import ConcurBrowserPanel from './ConcurBrowserPanel.svelte'
import type {Workspace} from './types'
vi.mock('../../lib/ipc',()=>({invokeSafe:vi.fn(),describeError:vi.fn(()=> '测试错误')}))
const mocked=vi.mocked(invokeSafe)
let mounted:ReturnType<typeof mount>|null=null
const data:Workspace={profile:{version:0,origin:'',account:'',report:{signature:'',fields:[]},expenses:{},expense_types:{},group_targets:{}},snapshot:{batch_id:0,snapshot_id:0,fingerprint:'synthetic',name:'内置测试',expenses:[{id:1,group_id:1,group_title:'样例本地费用',group_kind:'local_month',fields:{category_code:'city_transport',gross_amount:'1.11',currency_code:'CNY',transaction_date:'2026-06-02'},documents:[{id:1,name:'local-invoice.pdf',path:'synthetic.pdf',sha256:'synthetic',role:'main_invoice'}]}]},jobs:[]}
async function settle(){for(let n=0;n<6;n++){await Promise.resolve();await tick()}}
beforeEach(()=>{mocked.mockResolvedValue({ok:true,data:structuredClone(data)} as never)})
afterEach(async()=>{if(mounted)await unmount(mounted);mounted=null;mocked.mockReset();document.body.innerHTML=''})
describe('browser Concur entry',()=>{
  it('loads the packaged test snapshot without a real batch and never writes automatically',async()=>{
    mounted=mount(ConcurBrowserPanel,{target:document.body});await settle();
    expect(mocked).toHaveBeenCalledWith('get_concur_browser_workspace',{batchId:null});expect(mocked).toHaveBeenCalledTimes(1);
    expect(document.body.textContent).toContain('Concur 填报测试');expect(document.body.textContent).toContain('不会读取真实批次');
    const freeze=[...document.querySelectorAll('button')].find(b=>b.textContent?.includes('冻结测试计划'))!;
    expect(freeze.disabled).toBe(true);expect(document.querySelectorAll('section.block:not([hidden])').length).toBe(1);
  })
  it('exposes a dedicated test instance step and synthetic material names',async()=>{
    mounted=mount(ConcurBrowserPanel,{target:document.body});await settle();
    const tab=[...document.querySelectorAll('nav button')].find(b=>b.textContent?.includes('测试实例')) as HTMLButtonElement;tab.click();await settle();
    const visible=document.querySelector('section.block:not([hidden])')!;
    expect(visible.textContent).toContain('local-invoice.pdf');expect(visible.textContent).toContain('虚构数据');
    expect(document.querySelectorAll('section.block:not([hidden])').length).toBe(1);expect(mocked).toHaveBeenCalledTimes(1);
  })
  it('batch delivery uses the supplied reviewed batch, not the sample',async()=>{
    mounted=mount(ConcurBrowserPanel,{target:document.body,props:{batchId:8}});await settle();
    expect(mocked).toHaveBeenCalledWith('get_concur_browser_workspace',{batchId:8});
    expect(document.querySelector('header h2')?.textContent).toBe('上传到 Concur');
  })
  it('keeps the tested mapping when confirming and saving unchanged batch configuration',async()=>{
    const workspace=structuredClone(data)
    workspace.profile.version=7
    workspace.profile.account='synthetic@example.invalid'
    workspace.profile.origin='https://test.concursolutions.com'
    mocked.mockImplementation(async(command,args)=>{
      if(command==='get_concur_browser_workspace')return {ok:true,data:workspace} as never
      if(command==='concur_browser_control')return {ok:true,data:(args as {action:string}).action==='status'?{accounts:[workspace.profile.account],reports_read:true}:{account:workspace.profile.account,origin:workspace.profile.origin}} as never
      if(command==='save_concur_browser_profile')return {ok:true,data:structuredClone(workspace.profile)} as never
      throw new Error(`Unexpected command: ${command}`)
    })
    const click=async(text:string)=>{const button=[...document.querySelectorAll('button')].find(b=>b.textContent?.trim()===text)!;expect(button.disabled).toBe(false);button.click();await settle()}
    mounted=mount(ConcurBrowserPanel,{target:document.body,props:{batchId:8}});await settle()
    await click('读取账号及报销单');await click('确认此账号')
    expect(document.body.textContent).toContain('已有映射可确认后继续使用')
    const checkbox=[...document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')].find(input=>input.parentElement?.textContent?.includes('我已确认字段含义'))!
    checkbox.click();await settle();await click('保存映射')
    expect(mocked).toHaveBeenCalledWith('save_concur_browser_profile',{profile:workspace.profile})
    expect(document.body.textContent).toContain('映射未变化，已复用原版本')
    expect(document.querySelector('section.block:not([hidden])')?.textContent).toContain('交付计划')
    expect(mocked.mock.calls.some(([command])=>command==='run_concur_browser_step')).toBe(false)
  })
})
