use std::env;
use std::error::Error;
use std::path::PathBuf;

use invoice_assistant::commands::review::reanalyze_expense_categories_for_ledger;
use invoice_store::LedgerDb;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let database_path = args
        .next()
        .map(PathBuf::from)
        .ok_or("用法：reanalyze_batch_categories <ledger.db> <batch-id>")?;
    let batch_id = args
        .next()
        .ok_or("缺少 batch-id")?
        .to_string_lossy()
        .parse::<i64>()?;
    if args.next().is_some() {
        return Err("参数过多".into());
    }

    let db = LedgerDb::new(&database_path)?;
    let batch = db.get_batch(batch_id)?;
    let result = reanalyze_expense_categories_for_ledger(&db, batch_id)?;
    println!(
        "批次 {}（{}）费用类型重识别完成：扫描 {}，更新 {}，直接确认 {}，待用户确认建议 {}，仍为其他 {}",
        batch.id,
        batch.name,
        result.scanned_count,
        result.changed_count,
        result.confirmed_count,
        result.suggestion_count,
        result.remaining_unclassified_count,
    );
    Ok(())
}
