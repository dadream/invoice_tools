<script lang="ts">
  import {sources,type Form} from './types'
  let {form=$bindable(),scope,values={}}:{form:Form;scope:'report'|'expense';values?:Record<string,string[]>}=$props()
  const allowed=$derived(Object.entries(sources).filter(([key])=>scope==='report'?['report_name','report_date','comment','constant'].includes(key):!['report_name','report_date'].includes(key)))
</script>
<table>
  <thead><tr><th>读取到的 Concur 字段</th><th>软件稳定字段 / 固定值</th><th>填写规则</th></tr></thead>
  <tbody>{#each form.fields as field}<tr>
    <td>{field.label}{#if field.required}<span class="required"> 必填</span>{/if}</td>
    <td><select aria-label={`${field.label}来源`} bind:value={field.source}><option value="">不自动填写</option>{#each allowed as [key,label]}<option value={key}>{label}</option>{/each}</select></td>
    <td>{#if ['transaction_date','report_date'].includes(field.source)}<select aria-label={`${field.label}日期格式`} bind:value={field.format}><option value="">YYYY-MM-DD</option><option value="MM/DD/YYYY">MM/DD/YYYY</option></select>
    {:else if field.source==='constant'}
      {#if field.options.length}<select aria-label={`${field.label}固定值`} bind:value={field.constant}><option value="">请选择</option>{#each field.options as option}<option value={option}>{option}</option>{/each}</select>
      {:else}<input aria-label={`${field.label}固定值`} bind:value={field.constant} placeholder={field.control==='checkbox'?'true 或 false':'用户确认后保存'} />{/if}
    {:else if field.source && (field.options.length || field.control==='combobox')}
      {#each values[field.source]??[] as value}<label class="option">{value}<span>→</span>{#if field.options.length}
        <select aria-label={`${field.label}选项${value}`} bind:value={field.option_map[value]}><option value="">请选择对应选项</option>{#each field.options as option}<option value={option}>{option}</option>{/each}</select>
        {:else}<input aria-label={`${field.label}选项${value}`} bind:value={field.option_map[value]} placeholder="Concur 完整选项名称" />{/if}</label>{/each}
    {:else}<span class="hint">{field.source?'按原值填写':'由用户处理；必填项会阻止开始'}</span>{/if}</td>
  </tr>{/each}</tbody>
</table>
<style>
table{width:100%;border-collapse:collapse;table-layout:fixed}th,td{text-align:left;padding:.65rem .75rem;border-bottom:1px solid #d9dfe2;overflow-wrap:anywhere;vertical-align:top}th{background:#f1f3f4;color:#596870;font-size:.74rem}td{font-size:.85rem}select,input{box-sizing:border-box;width:100%;min-height:38px;padding:.45rem;border:1px solid #aebcc1;background:white;color:#17232d}.required{color:#ae5310;font-size:.72rem}.hint{color:#657078}.option{display:flex;align-items:center;gap:.4rem;margin-bottom:.3rem}.option input,.option select{flex:1;min-width:90px}
</style>
