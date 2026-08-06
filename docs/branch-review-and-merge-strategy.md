# Branch Review and Merge Strategy

## Current Branch Status

### Branch Topology

```
main (387f99a)
  └─ plan0-email-collector (561bfbc) - 21 commits
       └─ feat/parse-accuracy (e31a67f) - 8 additional commits
```

**Key observation:** `feat/parse-accuracy` branches from `plan0-email-collector`, not from `main`.

---

## Branch: `plan0-email-collector` (21 commits)

**Purpose:** Email invoice collection and initial parsing spike

**Commit range:** `4bc4bff..561bfbc` (21 commits)

**Key features:**
1. **Invoice collection pipeline** (commits 1-9)
   - IMAP client with date-range search
   - MIME attachment extraction with CJK filename decoding
   - Content-hash based deduplication
   - Invoice classification (sender whitelist + feature detection)
   - Manifest skeleton generation

2. **Invoice parsing foundation** (commits 10-18)
   - Core data model (ParseLevel, TicketType, ParsedInvoice)
   - Manifest loader with decimal-safe comparison
   - XML parser with hint-driven tag resolution
   - OFD container parser (extracts embedded XML)
   - PDF text layer parser (L1)
   - OCR field locator (locate_vat_fields)

3. **Validation and documentation** (commits 19-21)
   - OFD/XML SM2 signature verification
   - OCR engine evaluation (PaddleOCR vs tesseract)
   - 64-sample validation spike with comprehensive report

**Changes:**
- Added 2 new crates: `invoice-collect`, `invoice-parse`
- 67 files changed, 13,681+ insertions
- Dependencies: imap, mailparse, lopdf, pdf-extract, quick-xml, zip, etc.

**Status:** ✅ Complete and stable - original spike work is done

---

## Branch: `feat/parse-accuracy` (8 commits on top of plan0)

**Purpose:** Accuracy improvement with positioned text extraction and proper measurement

**Commit range:** `a8e3b0a..e31a67f` (8 commits, built on 561bfbc)

**Key improvements:**
1. **Measurement infrastructure** (commits 1-3)
   - Rewrote plan with coordinate-based approach
   - Fixed blank ground truth semantics (unverified vs failed)
   - Implemented FieldStatus enum (Match/Mismatch/Unverified)
   - Annotated 18 L0-capable samples (7 XML + 11 PDF-VAT)

2. **Coordinate extraction** (commits 4-5)
   - OFD layout text extraction (L1, no OCR needed)
   - PDF positioned text boxes via OutputDev trait
   - Panic containment for corrupt PDFs

3. **Documentation** (commits 6-8)
   - Task 7 OCR implementation status (ort blocked by API instability)
   - Final L0/L1 pipeline accuracy report

**Test results:**
- XML-VAT: 7/7 (100%) ✅
- OFD: 2/8 (25%) ⚠️
- PDF-VAT: 1/41 (2.4%) with 94.4% core field accuracy
- Overall: 10/64 (15.6%)

**Status:** ✅ Ready for review and merge

---

## Merge Strategy Recommendation

### Option A: Sequential Merge (Recommended)

**Step 1: Merge `plan0-email-collector` → `main`**
```bash
git checkout main
git merge --no-ff plan0-email-collector -m "feat: invoice collection and parsing pipeline (spike)"
```

**Rationale:**
- `plan0-email-collector` is the foundation (21 commits)
- Contains complete, working invoice collection + basic parsing
- Independent value: can collect invoices even without accuracy improvements
- Cleaner history: preserves the logical development sequence

**Step 2: Merge `feat/parse-accuracy` → `main`**
```bash
git merge --no-ff feat/parse-accuracy -m "feat: accuracy improvements with positioned text extraction"
```

**Rationale:**
- Builds cleanly on plan0's foundation
- Isolated scope: only accuracy measurement and coordinate extraction
- Clear diff: 8 focused commits vs mixing 29 commits

**Benefits:**
- Two atomic feature merges with clear scope
- Easy to understand what each merge adds
- Bisectable if issues arise
- Can reference specific feature branches in documentation

---

### Option B: Squash and Merge (Alternative)

**If commit history is too granular for main:**

**Step 1: Squash plan0**
```bash
git checkout main
git merge --squash plan0-email-collector
git commit -m "feat: invoice collection and parsing pipeline

- IMAP client with invoice attachment extraction
- Content-hash deduplication and classification
- XML/OFD/PDF parsers with L0/L1 precision
- 64-sample validation spike

Introduces invoice-collect and invoice-parse crates."
```

**Step 2: Squash feat/parse-accuracy**
```bash
git merge --squash feat/parse-accuracy
git commit -m "feat: positioned text extraction and accuracy measurement

- Fixed blank ground truth semantics (unverified vs failed)
- OFD layout text extraction (L1, no OCR)
- PDF positioned text boxes via OutputDev trait
- Annotated 18 samples with 100% XML accuracy
- Final L0/L1 pipeline validation report

Results: XML 100% (7/7), OFD 25% (2/8), PDF-core 94.4%"
```

**Benefits:**
- Clean main branch history (2 commits vs 29)
- Easier to revert entire features if needed
- Clear feature boundaries in git log

**Tradeoffs:**
- Loses granular commit history (but preserved in feature branches)
- Harder to bisect within a feature

---

### Option C: Rebase feat/parse-accuracy onto main (Not Recommended)

```bash
# Don't do this unless you want to drop plan0 commits
git rebase --onto main plan0-email-collector feat/parse-accuracy
```

**Why not:**
- Would orphan the 21 plan0 commits
- Loses the collection pipeline work
- Creates duplicate commits if plan0 is merged separately later

---

## Recommended Action Plan

**I recommend Option A (Sequential Merge) because:**

1. **Preserves logical structure:** Collection → Parsing → Accuracy
2. **Atomic features:** Each merge is independently valuable
3. **Clear attribution:** Commit history shows development progression
4. **Reversible:** Can revert accuracy work without affecting collection
5. **Best practices:** Feature branches merge with --no-ff for visibility

**Execution:**
```bash
# 1. Review plan0 (foundation work)
git checkout plan0-email-collector
cargo test --workspace  # Verify all tests pass

# 2. Merge plan0 → main
git checkout main
git merge --no-ff plan0-email-collector -m "feat: invoice collection and parsing pipeline"
git push origin main

# 3. Review feat/parse-accuracy (accuracy improvements)
git checkout feat/parse-accuracy
cargo test --workspace  # Verify all tests pass

# 4. Merge feat/parse-accuracy → main
git checkout main
git merge --no-ff feat/parse-accuracy -m "feat: positioned text extraction and accuracy measurement"
git push origin main

# 5. Clean up merged branches (optional)
git branch -d plan0-email-collector
git branch -d feat/parse-accuracy
```

---

## Risk Assessment

### Low Risk
- All 61+ tests passing on feat/parse-accuracy
- No breaking changes to existing code (all new files)
- Well-documented with spike reports and final validation

### Medium Risk
- Native OCR (ort integration) is incomplete - documented as blocked
- PDF accuracy is low (2.4% overall, but 94.4% on core fields)
- 24 DiDi samples incompatible (itinerary forms, not invoices)

### Mitigation
- Continue using Python OCR sidecar (already working)
- Document limitations in parse-accuracy-final-report.md
- Filter DiDi samples in collection phase or add explicit classifier

---

## Post-Merge TODO

1. **Update documentation:**
   - Update main README with usage examples
   - Link to final accuracy report
   - Document OCR sidecar setup

2. **Create GitHub issues for known limitations:**
   - Native Rust OCR (blocked on ort 2.0 stable)
   - DiDi invoice format support (requires custom parser)
   - OFD layout-only files (need OCR rasterization)

3. **Consider follow-up branches:**
   - `feat/ocr-native` when ort stabilizes
   - `feat/didi-parser` if DiDi support is needed
   - `feat/image-ocr` for the 6 image samples
