use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};

use invoice_assistant::commands::review::reanalyze_expense_categories_for_ledger;
use invoice_parse::model::TicketType as ParseTicketType;
use invoice_store::models::{ExpenseItem, InvoiceReviewUpdate, TicketType};
use invoice_store::LedgerDb;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let database_path = args
        .next()
        .map(PathBuf::from)
        .ok_or("用法：repair_batch_supporting <ledger.db> <batch-id>")?;
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
    let category_result = reanalyze_expense_categories_for_ledger(&db, batch_id)?;
    let reclassified_supporting = reclassify_parsed_supporting_expenses(&db, batch_id)?;
    let mut attached = 0usize;
    let mut duplicate_copies = 0usize;

    for pending in db
        .list_pending_invoice_documents(batch_id)?
        .into_iter()
        .filter(|document| matches!(document.status.as_str(), "pending" | "attached"))
    {
        let is_pending = pending.status == "pending";
        let path = Path::new(&pending.file_path);
        if is_pending
            && path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("ofd"))
        {
            let bytes = std::fs::read(path)?;
            let hints = invoice_parse::manifest::TagHints {
                invoice_number: Vec::new(),
                issue_date: Vec::new(),
                total_amount: Vec::new(),
                tax_amount: Vec::new(),
                tax_rate: Vec::new(),
                buyer_name: Vec::new(),
                seller_name: Vec::new(),
            };
            if let Ok(parsed) =
                invoice_parse::ofd::parse_invoice_ofd(&bytes, path, &hints, ParseTicketType::Other)
            {
                let matches = db
                    .list_invoices_by_batch(batch_id)?
                    .into_iter()
                    .filter(|invoice| {
                        invoice.invoice_number == parsed.invoice_number
                            && invoice.issue_date == parsed.issue_date
                            && invoice.amount == parsed.total_amount
                    })
                    .collect::<Vec<_>>();
                if matches.len() == 1 {
                    let expense = expense_for_invoice(&db, batch_id, matches[0].id)?;
                    db.assign_pending_invoice_document_with_audit(
                        pending.id,
                        expense.id,
                        "duplicate_copy",
                    )?;
                    duplicate_copies += 1;
                    continue;
                }
            }
        }

        if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("pdf"))
        {
            let bytes = std::fs::read(path)?;
            if let Ok(text) = invoice_parse::pdf::extract_text(&bytes, path) {
                if let Some(facts) = invoice_parse::pdf::extract_supporting_document_facts(&text) {
                    let matches = matching_expenses(&db, batch_id, &facts)?;
                    if matches.len() == 1 {
                        let expense = &matches[0];
                        if !is_pending && pending.assigned_expense_item_id != Some(expense.id) {
                            continue;
                        }
                        let role = match facts.kind.as_str() {
                            "ride_hailing_itinerary" => "itinerary",
                            "courier_detail" => "detail",
                            _ => "supporting",
                        };
                        if is_pending {
                            db.assign_pending_invoice_document_with_audit(
                                pending.id, expense.id, role,
                            )?;
                            attached += 1;
                        }
                        apply_facts(&db, expense, &facts, None)?;
                        continue;
                    }
                }
            }
        }

        // 该批次的图片结账单已经人工核对完整页：1500 元、上海、6 月 1 日入住。
        if pending.original_name == "2B5864CE_8B611E50_8F88266A00000000.png" {
            let matches = db
                .list_expense_items_by_batch(batch_id)?
                .into_iter()
                .filter(|expense| {
                    expense.category_code == "hotel"
                        && expense.gross_amount == rust_decimal::Decimal::new(150000, 2)
                })
                .collect::<Vec<_>>();
            if matches.len() == 1 {
                let expense = &matches[0];
                if !is_pending && pending.assigned_expense_item_id != Some(expense.id) {
                    continue;
                }
                if is_pending {
                    db.assign_pending_invoice_document_with_audit(
                        pending.id,
                        expense.id,
                        "supporting",
                    )?;
                    attached += 1;
                }
                let start = chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
                update_invoice_facts(
                    &db,
                    expense,
                    TicketType::Hotel,
                    Some("上海"),
                    None,
                    Some(start),
                )?;
                if expense.transaction_date != start
                    || expense.location.city_name.as_deref() != Some("上海")
                {
                    db.apply_supporting_document_facts_with_audit(expense.id, start, Some("上海"))?;
                }
            }
        }
    }

    let remaining = db
        .list_pending_invoice_documents(batch_id)?
        .into_iter()
        .filter(|document| document.status == "pending")
        .collect::<Vec<_>>();
    println!(
        "批次 {}（{}）处理完成：费用类型更新 {}，误解析材料纠正 {}，自动挂载 {}，重复原件 {}，剩余待判断 {}",
        batch.id,
        batch.name,
        category_result.changed_count,
        reclassified_supporting,
        attached,
        duplicate_copies,
        remaining.len()
    );
    for document in remaining {
        println!(
            "保留待判断：{}（{}）",
            document.original_name, document.detection_reason
        );
    }
    Ok(())
}

fn reclassify_parsed_supporting_expenses(
    db: &LedgerDb,
    batch_id: i64,
) -> Result<usize, Box<dyn Error>> {
    let expenses = db.list_expense_items_by_batch(batch_id)?;
    let mut changed = 0usize;
    for source in expenses
        .iter()
        .filter(|expense| expense.inclusion_status != "excluded")
        .filter(|expense| expense.category_code == "hotel")
    {
        let invoice = db
            .get_invoice(source.primary_invoice_id)?
            .ok_or_else(|| format!("发票 {} 不存在", source.primary_invoice_id))?;
        if invoice.invoice_number.trim().chars().count() > 12 {
            continue;
        }
        let path = Path::new(&invoice.file_path);
        if !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("pdf"))
        {
            continue;
        }
        let bytes = std::fs::read(path)?;
        let Some(text) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            invoice_parse::pdf::extract_text(&bytes, path)
        }))
        .ok()
        .and_then(Result::ok) else {
            continue;
        };
        let Some(facts) = invoice_parse::pdf::extract_supporting_document_facts(&text) else {
            continue;
        };
        if facts.kind != "hotel_folio" {
            continue;
        }
        let city = facts.cities.first().map(String::as_str);
        let matches = expenses
            .iter()
            .filter(|target| target.id != source.id)
            .filter(|target| target.inclusion_status == "included")
            .filter(|target| target.category_code == "hotel")
            .filter(|target| target.gross_amount == facts.total_amount)
            .filter(|target| city.map_or(true, |city| target.counterparty_name.contains(city)))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            continue;
        }
        let target = matches[0];
        db.reclassify_invoice_as_supporting_document_with_audit(
            source.primary_invoice_id,
            target.id,
        )?;
        apply_facts(db, target, &facts, None)?;
        changed += 1;
    }
    Ok(changed)
}

fn matching_expenses(
    db: &LedgerDb,
    batch_id: i64,
    facts: &invoice_parse::pdf::SupportingDocumentFacts,
) -> Result<Vec<ExpenseItem>, Box<dyn Error>> {
    Ok(db
        .list_expense_items_by_batch(batch_id)?
        .into_iter()
        .filter(|expense| expense.gross_amount == facts.total_amount)
        .filter(
            |expense| match (facts.kind.as_str(), facts.provider.as_str()) {
                ("ride_hailing_itinerary", "didi") => expense.counterparty_name.contains("滴滴"),
                ("ride_hailing_itinerary", "caocao") => expense.category_code == "city_transport",
                ("courier_detail", "courier") => expense.category_code == "courier_logistics",
                ("hotel_folio", "hotel") => expense.category_code == "hotel",
                _ => false,
            },
        )
        .collect())
}

fn expense_for_invoice(
    db: &LedgerDb,
    batch_id: i64,
    invoice_id: i64,
) -> Result<ExpenseItem, Box<dyn Error>> {
    db.list_expense_items_by_batch(batch_id)?
        .into_iter()
        .find(|expense| expense.primary_invoice_id == invoice_id)
        .ok_or_else(|| format!("发票 {invoice_id} 没有费用项").into())
}

fn apply_facts(
    db: &LedgerDb,
    expense: &ExpenseItem,
    facts: &invoice_parse::pdf::SupportingDocumentFacts,
    city_override: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let mut city = city_override
        .map(str::to_string)
        .or_else(|| facts.cities.first().cloned());
    match facts.kind.as_str() {
        "ride_hailing_itinerary" => {
            update_invoice_facts(
                db,
                expense,
                TicketType::CityTransport,
                city.as_deref(),
                facts.start_date,
                None,
            )?;
            enrich_hotels_from_trip_facts(db, expense.batch_id, facts)?;
        }
        "hotel_folio" => {
            if city.is_none() {
                city = expense
                    .counterparty_name
                    .contains("赤峰")
                    .then(|| "赤峰".to_string());
            }
            let inferred_city = city.as_deref();
            update_invoice_facts(
                db,
                expense,
                TicketType::Hotel,
                inferred_city,
                None,
                facts.start_date,
            )?;
        }
        _ => {}
    }
    if let Some(start) = facts.start_date {
        if expense.transaction_date != start
            || city
                .as_deref()
                .is_some_and(|value| expense.location.city_name.as_deref() != Some(value))
        {
            db.apply_supporting_document_facts_with_audit(expense.id, start, city.as_deref())?;
        }
    }
    Ok(())
}

fn enrich_hotels_from_trip_facts(
    db: &LedgerDb,
    batch_id: i64,
    facts: &invoice_parse::pdf::SupportingDocumentFacts,
) -> Result<(), Box<dyn Error>> {
    let (Some(start), Some(city)) = (facts.start_date, facts.cities.first()) else {
        return Ok(());
    };
    for hotel in db
        .list_expense_items_by_batch(batch_id)?
        .into_iter()
        .filter(|expense| expense.inclusion_status == "included")
        .filter(|expense| expense.category_code == "hotel")
        .filter(|expense| {
            facts
                .hotel_mentions
                .iter()
                .any(|mention| expense.counterparty_name.contains(mention))
        })
    {
        update_invoice_facts(db, &hotel, TicketType::Hotel, Some(city), None, Some(start))?;
        if hotel.transaction_date != start || hotel.location.city_name.as_deref() != Some(city) {
            db.apply_supporting_document_facts_with_audit(hotel.id, start, Some(city))?;
        }
    }
    Ok(())
}

fn update_invoice_facts(
    db: &LedgerDb,
    expense: &ExpenseItem,
    ticket_type: TicketType,
    city: Option<&str>,
    departure_date: Option<chrono::NaiveDate>,
    checkin_date: Option<chrono::NaiveDate>,
) -> Result<(), Box<dyn Error>> {
    let invoice = db
        .get_invoice(expense.primary_invoice_id)?
        .ok_or_else(|| format!("发票 {} 不存在", expense.primary_invoice_id))?;
    let next_city = city.map(str::to_string).or(invoice.city.clone());
    let next_departure = departure_date
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .or(invoice.departure_time);
    let next_checkin = checkin_date.or(invoice.checkin_date);
    if invoice.ticket_type == ticket_type
        && invoice.city == next_city
        && invoice.departure_time == next_departure
        && invoice.checkin_date == next_checkin
    {
        return Ok(());
    }
    db.update_invoice_review_fields(
        invoice.id,
        &InvoiceReviewUpdate {
            invoice_number: invoice.invoice_number,
            issue_date: invoice.issue_date,
            amount: invoice.amount,
            tax_amount: invoice.tax_amount,
            buyer_name: invoice.buyer_name,
            seller_name: invoice.seller_name,
            ticket_type,
            city: next_city,
            departure_time: next_departure,
            checkin_date: next_checkin,
        },
    )?;
    Ok(())
}
