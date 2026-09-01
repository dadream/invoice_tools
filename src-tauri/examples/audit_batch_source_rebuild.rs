use std::{env, error::Error, path::PathBuf};

use invoice_assistant::commands::review::audit_batch_source_rebuild_for_ledger;
use invoice_store::LedgerDb;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let database_path = args
        .next()
        .map(PathBuf::from)
        .ok_or("用法：audit_batch_source_rebuild <ledger.db> <batch-id>")?;
    let batch_id = args
        .next()
        .ok_or("缺少 batch-id")?
        .to_string_lossy()
        .parse::<i64>()?;
    if args.next().is_some() {
        return Err("参数过多".into());
    }

    let db = LedgerDb::new(database_path)?;
    let result = audit_batch_source_rebuild_for_ledger(&db, batch_id)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if !result.reproducible {
        return Err("批次自动字段不能完全由当前受管原件重建".into());
    }
    Ok(())
}
