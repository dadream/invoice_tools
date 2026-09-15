import {describe,it,expect} from 'vitest'
import {completed,progress,type Job} from './types'
const job=(status:string[]):Job=>({id:'synthetic',revision:0,test_mode:true,profile:{version:0,origin:'https://test.concursolutions.com',account:'synthetic@example.invalid',report:{signature:'',fields:[]},expenses:{},expense_types:{},group_targets:{}},snapshot:{batch_id:0,snapshot_id:0,fingerprint:'synthetic',name:'测试',expenses:[]},steps:status.map(s=>({key:s,kind:'attachment',label:'样例',status:s,message:'',remote_url:''})),gaps:[]})
describe('Concur test status',()=>{
  it('never treats untested or uncertain writes as passed',()=>{expect(completed(job([]))).toBe(false);expect(completed(job(['pending']))).toBe(false);expect(completed(job(['verified','writing']))).toBe(false)})
  it('counts append steps individually',()=>{expect(progress(job(['verified','verified','pending']))).toEqual({done:2,total:3});expect(completed(job(['verified','verified']))).toBe(true)})
  it('gaps prevent completion',()=>{const j=job(['verified']);j.gaps=['必填项缺少'];expect(completed(j)).toBe(false)})
})
