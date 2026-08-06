# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Chinese invoice processing system with two main components:
1. **invoice-collect**: IMAP-based email invoice collection with classification and deduplication
2. **invoice-parse**: Multi-format invoice parser supporting XML, OFD, and PDF with L0/L1/L2 precision levels

The codebase is a Rust workspace targeting local-first invoice extraction for expense reporting.

## Build and Test Commands

```bash
# Add cargo to PATH if needed
export PATH="$HOME/.cargo/bin:$PATH"

# Build entire workspace
cargo build --workspace

# Run all tests (64 tests across invoice-parse)
cargo test --workspace

# Run single test
cargo test --package invoice-parse --lib -- ocr::tests::locates_fields_in_inline_layout

# Run specific crate tests
cargo test -p invoice-parse
cargo test -p invoice-collect

# Build release binaries
cargo build --release --workspace
```

## Running the Tools

### invoice-collect (Email Collection)

```bash
# Requires INVOICE_IMAP_PASSWORD environment variable
export INVOICE_IMAP_PASSWORD="your-password-or-auth-code"

# Probe IMAP connection and list folders
cargo run -p invoice-collect -- probe <email@example.com> [since-date] [before-date]

# Collect invoices from INBOX in date range (default: 2026-06-01 to 2026-07-01)
cargo run -p invoice-collect -- collect <email@example.com> 2026-06-01 2026-07-15

# Audit: show classification stats without saving
cargo run -p invoice-collect -- audit <email@example.com>
```

### invoice-parse (Invoice Parsing)

```bash
# Explore XML structure (lists all leaf tags)
cargo run -p invoice-parse -- dump-tags <file.xml>

# Parse single invoice using manifest hints
cargo run -p invoice-parse -- parse-one <file.xml|file.ofd|file.pdf>

# Dump OFD container structure
cargo run -p invoice-parse -- dump-ofd <file.ofd>

# Check PDF text layer and extract text
cargo run -p invoice-parse -- dump-pdf <file.pdf>

# Extract positioned text boxes from PDF (L1)
cargo run -p invoice-parse -- dump-pdf-boxes <file.pdf>

# Verify OFD/XML signature (SM2 cryptography)
cargo run -p invoice-parse -- verify <file.ofd|file.xml>

# Verify all samples in fixtures/manifest.toml
cargo run -p invoice-parse -- verify-all

# Explore all XML samples and suggest tag hints
cargo run -p invoice-parse -- explore-xml
```

## Architecture

### Parse Levels (invoice-parse/src/model.rs)

The system uses a tiered precision model:

- **L0**: Structured data direct read (XML tags, embedded invoice data) - 100% confidence
- **L1**: PDF text layer + layout templates OR OFD layout XML extraction - 100% confidence
- **L2**: Local OCR with PaddleOCR (currently via Python sidecar) - variable confidence
- **L4**: Field conflicts detected, force manual review

Higher levels (L0) are always preferred. The parser attempts L0 → L1 → L2 fallback automatically.

### Core Data Model (invoice-parse/src/model.rs)

```rust
pub struct ParsedInvoice {
    pub invoice_number: String,           // Required: 20-digit format
    pub issue_date: NaiveDate,            // Required: YYYY-MM-DD
    pub total_amount: Decimal,            // Required: decimal-safe
    pub tax_amount: Option<Decimal>,      // Optional
    pub tax_rate: Option<Decimal>,        // Optional: 0.09 means 9%
    pub buyer_name: Option<String>,       // Optional
    pub seller_name: Option<String>,      // Optional
    pub ticket_type: TicketType,          // Rail/Flight/Hotel/CityTransport/Meal/Other
    pub parse_level: ParseLevel,          // L0/L1/L2/L4
    pub confidence: f32,                  // 0.0-1.0
    pub source_path: PathBuf,
}
```

### Module Responsibilities

**invoice-parse:**
- `model.rs`: Core types (ParsedInvoice, ParseLevel, TicketType, ParseError)
- `xml.rs`: Native XML parser with hint-driven tag resolution (L0)
- `ofd.rs`: OFD container parser, extracts embedded invoice XML (L0) or falls back to layout text (L1)
- `ofd_text.rs`: OFD layout XML text box extraction with coordinates (L1)
- `pdf.rs`: PDF text layer extraction with regex patterns (L0)
- `pdf_text.rs`: PDF positioned text boxes via OutputDev trait (L1), includes panic containment
- `ocr.rs`: Field locator from OCR text boxes, uses `locate_vat_fields()` with spatial pairing
- `manifest.rs`: Ground truth loader with FieldStatus (Match/Mismatch/Unverified)
- `verify.rs`: OFD/XML SM2 signature verification
- `report.rs`: Test report generation with field-level accuracy

**invoice-collect:**
- `imap_client.rs`: IMAP session with ID command and date-range search
- `extract.rs`: MIME attachment extraction with CJK filename decoding
- `classify.rs`: Invoice classifier (sender whitelist + feature detection)
- `dedupe.rs`: Content-hash based deduplication (SHA256)
- `store.rs`: Sample persistence with sanitized filenames
- `manifest_gen.rs`: Ground truth skeleton generation for manual annotation

### Tag Hints System

XML/OFD invoices use non-standard tag names. The manifest.toml contains `[hints]` mapping field names to candidate tags:

```toml
[hints]
invoice_number = ["Fphm", "InvoiceNumber"]
issue_date = ["Kprq"]
total_amount = ["Jshj"]
```

The parser tries each candidate in order until a match is found. This allows handling multiple invoice formats without hardcoding.

### Ground Truth and Verification

`fixtures/manifest.toml` contains annotated samples with expected values. Fields can be:
- **Populated**: Expected value present → verification checks Match/Mismatch
- **Blank/None**: No expected value → FieldStatus::Unverified (not counted as failure)

This enables incremental ground truth labeling. Run `verify-all` to measure accuracy against annotated samples.

### OCR Integration Status

**Current**: Python sidecar (`tools/ocr_sidecar.py`) using PaddleOCR
- Models: PP-OCRv6_small (det: 9.5 MiB, rec: 20.2 MiB)
- Dictionary: `models/ppocrv6_keys.txt` (18708 characters)
- Field locator: Native Rust `ocr::locate_vat_fields()` with spatial pairing

**Blocked**: Native Rust OCR via `ort` crate
- `ort = "2.0.0-rc.13"` has unstable API (GraphOptimizationLevel, Send+Sync trait bounds)
- Field locator is complete and tested (5/5 unit tests pass)
- Blocked on ecosystem maturity, revisit when ort 2.0 stable releases

See `docs/task-7-ocr-implementation-status.md` for details.

## Test Coverage

As of commit e31a67f:
- **64 passing tests** in invoice-parse
- **100% XML accuracy** (7/7 samples, L0)
- **25% OFD accuracy** (2/8 samples with embedded XML, L0)
- **94.4% PDF core field accuracy** (invoice_number, issue_date, total_amount via L0/L1)
- Optional fields (tax_amount, buyer_name, seller_name) need L2 OCR or better layout analysis

See `docs/parse-accuracy-final-report.md` for full validation results.

## Common Patterns

### Adding a New Invoice Format

1. Add parser module (e.g., `src/my_format.rs`)
2. Implement function returning `Result<ParsedInvoice, ParseError>`
3. Set appropriate `parse_level` and `confidence`
4. Add to `lib.rs` exports
5. Add command in `main.rs` for testing
6. Write unit tests with sample data

### Updating Tag Hints

1. Run `explore-xml` to see all tag names across XML samples
2. Update `fixtures/manifest.toml` `[hints]` section
3. Test with `parse-one` on affected samples
4. Run `verify-all` to measure impact

### Adding Ground Truth

1. Manually inspect invoice file for expected values
2. Add or update sample entry in `fixtures/manifest.toml`
3. Use `Option<T>` fields (omit line for unverified fields)
4. Run `verify-all` to check Match/Mismatch/Unverified status

## Important Constraints

- **Decimal safety**: Use `rust_decimal::Decimal` for all monetary amounts, never `f64`
- **Date format**: `chrono::NaiveDate` for issue dates, stored as YYYY-MM-DD
- **Character encoding**: CJK filenames require proper decoding (see `extract.rs`)
- **Panic containment**: PDF parsing via `pdf-extract` can panic, use `catch_unwind` (see `pdf_text.rs`)
- **ParseLevel semantics**: L0 confidence is always 1.0 (deterministic), L2 inherits OCR confidence

## Key Files to Check

- `fixtures/manifest.toml`: Ground truth annotations and tag hints
- `docs/parse-accuracy-final-report.md`: Current accuracy metrics and MVP readiness
- `docs/superpowers/plans/2026-08-05-parse-accuracy.md`: Full 8-task execution plan
- `.env.sample`: Template for IMAP credentials (never commit real `.env.local`)

## Dependencies of Note

- `pdf-extract`: Can panic on malformed PDFs, wrap in `catch_unwind`
- `lopdf`: Alternative PDF library, used for low-level inspection
- `quick-xml`: Lenient XML parser, strips namespaces automatically
- `zip`: OFD containers are ZIP archives with specific structure
- `smcrypto`: SM2/SM3 Chinese cryptography standards for signature verification
- `euclid`: Geometry types for PDF text box coordinates (must match pdf-extract's 0.20)

## Memory Notes

Memory files in `.claude/projects/-home-holo-work-tools/memory/`:
- `no-read-env-local.md`: Never read real IMAP credentials file `.env.local`
