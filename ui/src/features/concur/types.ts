export interface Field {label:string;required:boolean;control:string;options:string[];source:string;constant:string;option_map:Record<string,string>;format:string}
export interface Form {signature:string;fields:Field[]}
export interface Profile {version:number;origin:string;account:string;report:Form;expenses:Record<string,Form>;expense_types:Record<string,string>;group_targets:Record<string,string>}
export interface Expense {id:number;group_id:number;group_title:string;group_kind:string;fields:Record<string,string>;documents:{id:number;name:string;path:string;sha256:string;role:string}[]}
export interface Snapshot {batch_id:number;snapshot_id:number;fingerprint:string;name:string;expenses:Expense[]}
export interface Step {key:string;kind:string;label:string;status:string;message:string;remote_url:string}
export interface Job {id:string;revision:number;test_mode:boolean;profile:Profile;snapshot:Snapshot;steps:Step[];gaps:string[]}
export interface Workspace {profile:Profile;snapshot:Snapshot;jobs:Job[]}
export const sources:Record<string,string>={report_name:'报销单名称',report_date:'报销单日期',transaction_date:'实际日期',gross_amount:'实际金额',description:'费用描述',counterparty_name:'交易方',city_name:'发生城市',province_name:'省份',currency_code:'币种',payment_method:'付款方式',tax_amount:'票面税额（须确认申报语义）',comment:'恢复识别标记',constant:'固定值 / 本次补充值'};
export const categories:Record<string,string>={rail:'火车',flight:'飞机',hotel:'住宿',city_transport:'市内交通',meal:'餐饮',courier_logistics:'快递物流',other:'未分类'};
export const stepName:Record<string,string>={report:'草稿创建',expense:'费用创建',attachment:'附件追加'};
export function progress(job:Job){return {done:job.steps.filter(s=>s.status==='verified').length,total:job.steps.length};}
export function completed(job:Job){const p=progress(job);return p.total>0&&p.done===p.total&&job.gaps.length===0;}
