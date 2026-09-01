use std::env;
use std::error::Error;
use std::path::PathBuf;

use invoice_assistant::commands::review::recompute_batch_grouping_for_ledger;
use invoice_store::LedgerDb;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let database_path = args
        .next()
        .map(PathBuf::from)
        .ok_or("用法：recompute_batch_grouping <ledger.db> <batch-id>")?;
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
    let result = recompute_batch_grouping_for_ledger(&db, batch_id)?;

    println!(
        "批次 {}（{}）归组更新完成：费用 {}，归组 {}，差旅 {}，路线未解析 {}",
        batch.id,
        batch.name,
        result.invoice_count,
        result.group_count,
        result.business_trip_count,
        result.unresolved_transport_count
    );
    Ok(())
}
