use crate::field_extractor;
use crate::model::{ParseError, ParseLevel, ParsedInvoice, TicketType};
use crate::xml::{parse_amount, parse_date, parse_tax_rate};
use ort::{
    execution_providers::CPUExecutionProvider,
    session::builder::{GraphOptimizationLevel, SessionBuilder},
};
use paddle_ocr_rs::ocr_lite::OcrLite;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// OCR 识别出的一个文本框。坐标为左上角原点的像素值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextBox {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub confidence: f32,
}

impl TextBox {
    fn center_y(&self) -> f32 {
        self.y + self.height / 2.0
    }
    fn right(&self) -> f32 {
        self.x + self.width
    }
}

pub const OCR_RUNTIME_FILE: &str = "onnxruntime.dll";
pub const OCR_PROVIDER_SHARED_FILE: &str = "onnxruntime_providers_shared.dll";
pub const OCR_DETECTION_MODEL_FILE: &str = "models/ch_PP-OCRv5_det_mobile.onnx";
pub const OCR_CLASSIFICATION_MODEL_FILE: &str = "models/ch_ppocr_mobile_v2.0_cls_mobile.onnx";
pub const OCR_RECOGNITION_MODEL_FILE: &str = "models/ch_PP-OCRv5_rec_mobile.onnx";

const OCR_RUNTIME_SHA256: &str = "579B636403983254346A5C1D80BD28F1519CD1E284CD204F8D4FF41F8D711559";
const OCR_PROVIDER_SHARED_SHA256: &str =
    "BA00EA1EF846C9B909C7854BC56C51051A20F9773B3E1153DDA118D4B85D0B93";
const OCR_DETECTION_SHA256: &str =
    "4D97C44A20D30A81AAD087D6A396B08F786C4635742AFC391F6621F5C6AE78AE";
const OCR_CLASSIFICATION_SHA256: &str =
    "E47ACEDF663230F8863FF1AB0E64DD2D82B838FCEB5957146DAB185A89D6215C";
const OCR_RECOGNITION_SHA256: &str =
    "5825FC7EBF84AE7A412BE049820B4D86D77620F204A041697B0494669B1742C5";
const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_IMAGE_SIDE: u32 = 12_000;
const MAX_IMAGE_PIXELS: u64 = 50_000_000;
// 检测网络会在推理前把更大图片按比例缩小。1024–1600 会让当前发票 golden
// 低于 0.85 置信度；1800 保持字段/置信度门禁，并比原 2000 参数降低扫描 PDF 峰值。
const OCR_DETECTION_MAX_SIDE: u32 = 1_800;

#[derive(Debug, thiserror::Error)]
pub enum OfflineOcrError {
    #[error("离线 OCR 组件缺失：{component}；请重新解压完整便携包")]
    AssetMissing { component: &'static str },
    #[error("离线 OCR 组件校验失败：{component}；请重新下载便携包")]
    AssetIntegrity { component: &'static str },
    #[error("图片文件超过 25 MB 限制")]
    ImageFileTooLarge,
    #[error("图片尺寸超过 12000 像素或 5000 万像素限制")]
    ImageDimensionsTooLarge,
    #[error("图片文件损坏或格式不受支持")]
    ImageDecode,
    #[error("离线 OCR 引擎初始化失败；请重新解压完整便携包")]
    EngineInitialization,
    #[error("离线 OCR 识别失败；请确认图片清晰且未损坏")]
    Inference,
    #[error("离线 OCR 暂时不可用；请重启应用后重试")]
    EngineUnavailable,
    #[error("图片 OCR 未找到必需字段 {field}")]
    MissingField { field: String },
    #[error("图片 OCR 字段 {field} 格式无效")]
    InvalidField { field: String },
}

struct CachedOcrEngine {
    asset_dir: PathBuf,
    engine: OcrLite,
    _dll_directory: crate::windows_security::DllDirectoryCookie,
}

static OCR_ENGINE: OnceLock<Mutex<CachedOcrEngine>> = OnceLock::new();
static OCR_INIT_LOCK: Mutex<()> = Mutex::new(());

fn sha256_file(path: &Path) -> Result<String, OfflineOcrError> {
    let mut file = File::open(path).map_err(|_| OfflineOcrError::EngineInitialization)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| OfflineOcrError::EngineInitialization)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:X}", hasher.finalize()))
}

fn verified_asset(
    asset_dir: &Path,
    relative: &'static str,
    expected_sha256: &str,
) -> Result<PathBuf, OfflineOcrError> {
    let path = asset_dir.join(relative);
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| OfflineOcrError::AssetMissing {
        component: relative,
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(OfflineOcrError::AssetIntegrity {
            component: relative,
        });
    }
    let actual = sha256_file(&path)?;
    if actual != expected_sha256 {
        return Err(OfflineOcrError::AssetIntegrity {
            component: relative,
        });
    }
    Ok(path)
}

fn path_text(path: &Path) -> Result<&str, OfflineOcrError> {
    path.to_str().ok_or(OfflineOcrError::EngineInitialization)
}

fn configure_ocr_session(builder: SessionBuilder) -> Result<SessionBuilder, ort::Error> {
    builder
        .with_optimization_level(GraphOptimizationLevel::Level2)?
        .with_intra_threads(2)?
        .with_inter_threads(1)?
        .with_memory_pattern(false)?
        .with_execution_providers([CPUExecutionProvider::default()
            .with_arena_allocator(false)
            .build()])
}

fn initialized_engine(
    asset_dir: &Path,
) -> Result<&'static Mutex<CachedOcrEngine>, OfflineOcrError> {
    let canonical_dir = asset_dir
        .canonicalize()
        .map_err(|_| OfflineOcrError::AssetMissing {
            component: OCR_RUNTIME_FILE,
        })?;

    if let Some(cache) = OCR_ENGINE.get() {
        let cached = cache
            .lock()
            .map_err(|_| OfflineOcrError::EngineUnavailable)?;
        if cached.asset_dir != canonical_dir {
            return Err(OfflineOcrError::EngineUnavailable);
        }
        drop(cached);
        return Ok(cache);
    }

    let init_guard = OCR_INIT_LOCK
        .lock()
        .map_err(|_| OfflineOcrError::EngineUnavailable)?;
    if OCR_ENGINE.get().is_none() {
        let runtime = verified_asset(&canonical_dir, OCR_RUNTIME_FILE, OCR_RUNTIME_SHA256)?;
        let _provider_shared = verified_asset(
            &canonical_dir,
            OCR_PROVIDER_SHARED_FILE,
            OCR_PROVIDER_SHARED_SHA256,
        )?;
        let detection = verified_asset(
            &canonical_dir,
            OCR_DETECTION_MODEL_FILE,
            OCR_DETECTION_SHA256,
        )?;
        let classification = verified_asset(
            &canonical_dir,
            OCR_CLASSIFICATION_MODEL_FILE,
            OCR_CLASSIFICATION_SHA256,
        )?;
        let recognition = verified_asset(
            &canonical_dir,
            OCR_RECOGNITION_MODEL_FILE,
            OCR_RECOGNITION_SHA256,
        )?;
        let dll_directory = crate::windows_security::add_verified_dll_directory(&canonical_dir)
            .map_err(|_| OfflineOcrError::EngineInitialization)?;

        ort::init_from(path_text(&runtime)?)
            .commit()
            .map_err(|_| OfflineOcrError::EngineInitialization)?;
        let mut engine = OcrLite::new();
        engine
            .init_models_custom(
                path_text(&detection)?,
                path_text(&classification)?,
                path_text(&recognition)?,
                configure_ocr_session,
            )
            .map_err(|_| OfflineOcrError::EngineInitialization)?;
        OCR_ENGINE
            .set(Mutex::new(CachedOcrEngine {
                asset_dir: canonical_dir.clone(),
                engine,
                _dll_directory: dll_directory,
            }))
            .map_err(|_| OfflineOcrError::EngineUnavailable)?;
    }
    drop(init_guard);
    OCR_ENGINE.get().ok_or(OfflineOcrError::EngineUnavailable)
}

fn text_block_to_box(block: paddle_ocr_rs::ocr_result::TextBlock) -> Option<TextBox> {
    let min_x = block.box_points.iter().map(|point| point.x).min()?;
    let max_x = block.box_points.iter().map(|point| point.x).max()?;
    let min_y = block.box_points.iter().map(|point| point.y).min()?;
    let max_y = block.box_points.iter().map(|point| point.y).max()?;
    let text = block.text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(TextBox {
        text,
        x: min_x as f32,
        y: min_y as f32,
        width: max_x.saturating_sub(min_x) as f32,
        height: max_y.saturating_sub(min_y) as f32,
        confidence: block
            .text_score
            .clamp(0.0, 1.0)
            .min(block.box_score.clamp(0.0, 1.0)),
    })
}

fn validate_image_dimensions(dimensions: (u32, u32)) -> Result<(), OfflineOcrError> {
    let pixels = u64::from(dimensions.0) * u64::from(dimensions.1);
    if dimensions.0 == 0
        || dimensions.1 == 0
        || dimensions.0 > MAX_IMAGE_SIDE
        || dimensions.1 > MAX_IMAGE_SIDE
        || pixels > MAX_IMAGE_PIXELS
    {
        return Err(OfflineOcrError::ImageDimensionsTooLarge);
    }
    Ok(())
}

fn recognize_rgb(
    image: &image::RgbImage,
    asset_dir: &Path,
) -> Result<Vec<TextBox>, OfflineOcrError> {
    let cache = initialized_engine(asset_dir)?;
    let mut cached = cache
        .lock()
        .map_err(|_| OfflineOcrError::EngineUnavailable)?;
    let result = cached
        .engine
        .detect_angle_rollback(
            image,
            50,
            OCR_DETECTION_MAX_SIDE,
            0.5,
            0.3,
            1.6,
            true,
            false,
            0.80,
        )
        .map_err(|_| OfflineOcrError::Inference)?;
    Ok(result
        .text_blocks
        .into_iter()
        .filter_map(text_block_to_box)
        .collect())
}

fn decode_image_with_guessed_format(image_path: &Path) -> Result<image::RgbImage, OfflineOcrError> {
    image::ImageReader::open(image_path)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|_| OfflineOcrError::ImageDecode)?
        .decode()
        .map_err(|_| OfflineOcrError::ImageDecode)
        .map(|image| image.to_rgb8())
}

pub fn recognize_offline(
    image_path: &Path,
    asset_dir: &Path,
) -> Result<Vec<TextBox>, OfflineOcrError> {
    let metadata =
        std::fs::symlink_metadata(image_path).map_err(|_| OfflineOcrError::ImageDecode)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(OfflineOcrError::ImageDecode);
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(OfflineOcrError::ImageFileTooLarge);
    }

    let dimensions = image::ImageReader::open(image_path)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|_| OfflineOcrError::ImageDecode)?
        .into_dimensions()
        .map_err(|_| OfflineOcrError::ImageDecode)?;
    validate_image_dimensions(dimensions)?;

    // 收集/解压阶段可能使用规范化候选后缀，不能再按扩展名决定解码器。
    // 与上面的尺寸探测一致，正式解码也必须根据文件魔数识别实际格式。
    let image = decode_image_with_guessed_format(image_path)?;
    recognize_rgb(&image, asset_dir)
}

pub fn parse_invoice_image(
    image_path: &Path,
    asset_dir: &Path,
) -> Result<ParsedInvoice, OfflineOcrError> {
    let boxes = recognize_offline(image_path, asset_dir)?;
    locate_vat_fields(&boxes, image_path, ParseLevel::L2).map_err(|error| match error {
        ParseError::MissingField { field, .. } => OfflineOcrError::MissingField { field },
        ParseError::UnparseableValue { field, .. } => OfflineOcrError::InvalidField { field },
        _ => OfflineOcrError::Inference,
    })
}

/// 对内存中的图片执行与文件路径入口相同的尺寸门禁和离线 OCR。
/// PDF 渲染器和诊断工具使用该入口，避免为每一页生成临时文件。
pub(crate) fn recognize_offline_bytes(
    image_bytes: &[u8],
    asset_dir: &Path,
) -> Result<Vec<TextBox>, OfflineOcrError> {
    if image_bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(OfflineOcrError::ImageFileTooLarge);
    }
    let dimensions = image::ImageReader::new(Cursor::new(image_bytes))
        .with_guessed_format()
        .map_err(|_| OfflineOcrError::ImageDecode)?
        .into_dimensions()
        .map_err(|_| OfflineOcrError::ImageDecode)?;
    validate_image_dimensions(dimensions)?;
    let image = image::load_from_memory(image_bytes)
        .map_err(|_| OfflineOcrError::ImageDecode)?
        .to_rgb8();
    recognize_rgb(&image, asset_dir)
}

pub(crate) fn parse_invoice_image_bytes(
    image_bytes: &[u8],
    source_path: &Path,
    asset_dir: &Path,
) -> Result<ParsedInvoice, OfflineOcrError> {
    let boxes = recognize_offline_bytes(image_bytes, asset_dir)?;
    locate_vat_fields(&boxes, source_path, ParseLevel::L2).map_err(|error| match error {
        ParseError::MissingField { field, .. } => OfflineOcrError::MissingField { field },
        ParseError::UnparseableValue { field, .. } => OfflineOcrError::InvalidField { field },
        _ => OfflineOcrError::Inference,
    })
}
/// 同一行的判定阈值：两个框的垂直中心相差不超过这个像素数
const SAME_LINE_TOLERANCE: f32 = 15.0;

/// 在文本框集合中查找某个字段的值。
///
/// 先试同框内提取（标签后面就是值），失败则找同一行、位于标签右侧、
/// 且水平距离最近的框。
///
/// 返回 (值文本, 该值所在框的置信度)
fn find_value(
    boxes: &[TextBox],
    labels: &[&str],
    validate: impl Fn(&str) -> bool,
) -> Option<(String, f32)> {
    for label in labels {
        for b in boxes.iter().filter(|b| b.text.contains(label)) {
            // 情况 1：标签和值在同一个框
            if let Some(rest) = b.text.split(label).nth(1) {
                let candidate = rest.trim_start_matches([' ', '：', ':', '\u{3000}']).trim();
                if !candidate.is_empty() && validate(candidate) {
                    return Some((candidate.to_string(), b.confidence));
                }
            }

            // 情况 2：值在同一行的右邻框
            // 允许轻微重叠（-5.0）以应对 PDF 文本框宽度计算的不精确
            let mut right_neighbors: Vec<&TextBox> = boxes
                .iter()
                .filter(|other| {
                    (other.center_y() - b.center_y()).abs() <= SAME_LINE_TOLERANCE
                        && other.x >= b.right() - 5.0
                        && !std::ptr::eq(*other, b)
                })
                .collect();
            right_neighbors.sort_by(|p, q| p.x.partial_cmp(&q.x).unwrap());

            for n in right_neighbors {
                let candidate = n.text.trim();
                if !candidate.is_empty() && validate(candidate) {
                    return Some((candidate.to_string(), n.confidence));
                }
            }
        }
    }

    None
}

fn looks_like_digits(s: &str) -> bool {
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    digits >= 8
}

fn looks_like_date(s: &str) -> bool {
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    // 支持日期（6-8位）或日期+时间（14位左右）
    (6..=8).contains(&digits) || (12..=20).contains(&digits)
}

fn looks_like_amount(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_digit() || "￥¥,. 元".contains(c))
}

/// 从包含中文大写金额的混合文本中提取阿拉伯数字金额。
///
/// 例如 "壹拾伍圆整 ¥15.00" → Some("15.00")
///      "（小写）¥15.00"   → Some("15.00")
///      "（小写）143.40¥"  → Some("143.40")
///      "合计20.98元"      → Some("20.98")
fn extract_amount_from_mixed(text: &str) -> Option<String> {
    // 策略 1：找最后一个 ¥ 或 ￥，优先取其后的数字串，如果后面为空则取前面的
    if let Some((pos, ch)) = text.char_indices().rfind(|(_, c)| *c == '¥' || *c == '￥') {
        let after = text[pos + ch.len_utf8()..].trim();
        let num_after: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
            .filter(|c| *c != ',')
            .collect();
        if !num_after.is_empty() {
            return Some(num_after);
        }

        // ¥ 后面为空，尝试取前面的数字（逆序扫描到第一个非数字字符）
        let before = text[..pos].trim();
        let num_before: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
            .filter(|c| *c != ',')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if !num_before.is_empty() {
            return Some(num_before);
        }
    }

    // 策略 2：找「元」，取其前面的数字（逆序扫描到第一个非数字字符）
    if let Some(pos) = text.rfind('元') {
        let before = text[..pos].trim();
        let num: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
            .filter(|c| *c != ',')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if !num.is_empty() {
            return Some(num);
        }
    }

    None
}

/// 在文本框集合中查找金额字段，支持三级降级：
/// 1. 标准逻辑（同框或同行右邻，纯数字/符号）
/// 2. 同框/同行右邻混合文本中提取（"壹拾伍圆整 ¥15.00" → "15.00"）
/// 3. 跨行：label 下方 NEXT_LINE_TOLERANCE 像素内的相邻框
fn find_amount_value(boxes: &[TextBox], labels: &[&str]) -> Option<(String, f32)> {
    const NEXT_LINE_TOLERANCE: f32 = 30.0;

    // 级别 1：标准逻辑
    if let Some(result) = find_value(boxes, labels, looks_like_amount) {
        return Some(result);
    }

    // 级别 2 & 3：逐标签尝试混合文本提取和跨行搜索
    for label in labels {
        for b in boxes.iter().filter(|b| b.text.contains(label)) {
            // 2a：同框混合（"（小写）¥15.00" 整体在一个框）
            if let Some(amount) = extract_amount_from_mixed(&b.text) {
                return Some((amount, b.confidence));
            }

            // 2b：同行右侧框包含混合文本
            let mut right_neighbors: Vec<&TextBox> = boxes
                .iter()
                .filter(|other| {
                    (other.center_y() - b.center_y()).abs() <= SAME_LINE_TOLERANCE
                        && other.x >= b.right() - 5.0
                        && !std::ptr::eq(*other, b)
                })
                .collect();
            right_neighbors.sort_by(|p, q| p.x.partial_cmp(&q.x).unwrap());
            for n in &right_neighbors {
                if let Some(amount) = extract_amount_from_mixed(&n.text) {
                    return Some((amount, n.confidence));
                }
            }

            // 3：跨行——在 label 下方 NEXT_LINE_TOLERANCE px 内搜索
            let label_bottom = b.y + b.height;
            let mut below: Vec<&TextBox> = boxes
                .iter()
                .filter(|other| {
                    other.y >= label_bottom - 5.0
                        && other.y <= label_bottom + NEXT_LINE_TOLERANCE
                        && !std::ptr::eq(*other, b)
                })
                .collect();
            below.sort_by(|p, q| {
                p.y.partial_cmp(&q.y)
                    .unwrap()
                    .then(p.x.partial_cmp(&q.x).unwrap())
            });
            for n in &below {
                let t = n.text.trim();
                if looks_like_amount(t) {
                    return Some((t.to_string(), n.confidence));
                }
                if let Some(amount) = extract_amount_from_mixed(t) {
                    return Some((amount, n.confidence));
                }
            }
        }
    }

    // 只在页面文本中完全没有金额标签时启用无标签兜底。若标签存在但候选值
    // 距离过远，应保留为解析失败，避免把页面上的任意货币金额静默当作总额。
    if labels
        .iter()
        .any(|label| boxes.iter().any(|candidate| candidate.text.contains(label)))
    {
        return None;
    }

    // 部分数电票 OFD 把「价税合计/小写」画成矢量轮廓，只保留 ￥/¥ 和数值文本。
    // 该兜底只能用于 total_amount，不能进入发票号、税额、税率或名称的通用查找。
    // 碎片合并后可能形成一个「￥47.40」框；若有多个币种金额，页面最下方通常是总额。
    let mut currency_amounts = boxes
        .iter()
        .filter_map(|candidate| {
            if candidate.text.contains(['￥', '¥']) {
                extract_amount_from_mixed(&candidate.text)
                    .map(|amount| (candidate, amount, candidate.confidence))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    currency_amounts.sort_by(|left, right| right.0.y.partial_cmp(&left.0.y).unwrap());
    if let Some((_, amount, confidence)) = currency_amounts.first() {
        return Some((amount.clone(), *confidence));
    }

    // 碎片尚未合并时，币种符号和金额可能是两个 Boundary 完全重叠的 TextObject。
    for symbol in boxes.iter().filter(|b| matches!(b.text.trim(), "￥" | "¥")) {
        let mut overlapping_amounts = boxes
            .iter()
            .filter(|candidate| {
                !std::ptr::eq(*candidate, symbol)
                    && (candidate.x - symbol.x).abs() <= 3.0
                    && (candidate.y - symbol.y).abs() <= 3.0
                    && looks_like_amount(candidate.text.trim())
            })
            .collect::<Vec<_>>();
        overlapping_amounts.sort_by(|left, right| {
            let left_distance = (left.x - symbol.x).abs() + (left.y - symbol.y).abs();
            let right_distance = (right.x - symbol.x).abs() + (right.y - symbol.y).abs();
            left_distance.partial_cmp(&right_distance).unwrap()
        });
        if let Some(amount) = overlapping_amounts.first() {
            return Some((amount.text.trim().to_string(), amount.confidence));
        }
    }
    None
}

fn looks_like_rate(s: &str) -> bool {
    s.contains('%') || s.chars().any(|c| c.is_ascii_digit())
}

fn any_text(_s: &str) -> bool {
    true
}

/// 在指定列中查找数值。
///
/// 增值税发票是表格版式：「税额」「金额」等字段是列标题，
/// 值在该列下方的若干行中。本函数先找列标题框，
/// 再在标题框的 x 坐标±容差范围内、y > 标题底部的框中找第一个满足校验的值。
///
/// 返回 (值文本, 置信度)
fn find_in_column(
    boxes: &[TextBox],
    column_labels: &[&str],
    validate: impl Fn(&str) -> bool,
    x_tolerance: f32,
) -> Option<(String, f32)> {
    for label in column_labels {
        for header in boxes.iter().filter(|b| b.text.contains(label)) {
            // 跳过合并表头（如「税率/征收率税额」）。不能使用固定像素宽度，
            // 因为同一「税额」表头在高 DPI 扫描页中也可能超过 80px。
            if header.text.chars().filter(|c| !c.is_whitespace()).count() > 4 {
                continue;
            }
            let col_x = header.x;
            let col_bottom = header.y + header.height;

            // 在该列（x 坐标±容差）下方找第一个满足校验的框
            let mut candidates: Vec<&TextBox> = boxes
                .iter()
                .filter(|b| {
                    b.y > col_bottom
                        && (b.x - col_x).abs() <= x_tolerance
                        && !std::ptr::eq(*b, header)
                })
                .collect();
            candidates.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap());

            for c in candidates {
                let text = c.text.trim();
                if !text.is_empty() && validate(text) {
                    return Some((text.to_string(), c.confidence));
                }
            }
        }
    }
    None
}

/// 在指定列中查找数值并求和（支持多行明细）。
///
/// 与 find_in_column 类似，但收集所有满足校验的数值框并求和，
/// 用于支持多商品明细的税额汇总。
///
/// 返回 (求和结果文本, 最小置信度)
fn find_in_column_sum(
    boxes: &[TextBox],
    column_labels: &[&str],
    validate: impl Fn(&str) -> bool,
    x_tolerance: f32,
) -> Option<(String, f32)> {
    use rust_decimal::Decimal;
    use std::str::FromStr;

    for label in column_labels {
        for header in boxes.iter().filter(|b| b.text.contains(label)) {
            if header.text.chars().filter(|c| !c.is_whitespace()).count() > 4 {
                continue;
            }
            let col_x = header.x;
            let col_bottom = header.y + header.height;

            let mut candidates: Vec<&TextBox> = boxes
                .iter()
                .filter(|b| {
                    b.y > col_bottom
                        && (b.x - col_x).abs() <= x_tolerance
                        && !std::ptr::eq(*b, header)
                })
                .collect();
            candidates.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap());

            // 收集所有满足校验的数值框
            let mut sum = Decimal::ZERO;
            let mut min_confidence = f32::INFINITY;
            let mut count = 0;

            for c in candidates {
                let text = c.text.trim();
                // 跳过包含"合计"的行（总计行，不应计入明细求和）
                if text.contains("合计") || text.contains("合 计") {
                    continue;
                }
                if !text.is_empty() && validate(text) {
                    // 尝试解析为金额
                    let cleaned = text.replace(['￥', '¥', ','], "");
                    if let Ok(val) = Decimal::from_str(&cleaned) {
                        // 检查同行是否有"合"或"计"字（可能是拆分的合计行标记）
                        let same_line_boxes: Vec<&TextBox> = boxes
                            .iter()
                            .filter(|other| {
                                (other.center_y() - c.center_y()).abs() <= SAME_LINE_TOLERANCE
                                    && !std::ptr::eq(*other, c)
                            })
                            .collect();
                        let has_total_marker = same_line_boxes
                            .iter()
                            .any(|b| b.text.contains("合") || b.text.contains("计"));

                        if !has_total_marker {
                            sum += val;
                            min_confidence = min_confidence.min(c.confidence);
                            count += 1;
                        }
                    }
                }
            }

            if count > 0 {
                return Some((sum.to_string(), min_confidence));
            }
        }
    }
    None
}

type FieldWithConfidence = Option<(String, f32)>;

/// 在左右两列区块中查找「名称：」字段。
///
/// 增值税发票的购销方信息是左右分栏：买方在左侧（x < 中线），
/// 卖方在右侧（x > 中线）。每个区块内都有「名称：」标签，
/// 本函数先按 x 坐标判定买方/卖方区块，再在对应区块内找「名称：」右侧的值。
///
/// 返回 (buyer_name, buyer_conf, seller_name, seller_conf)
fn find_buyer_seller_names(boxes: &[TextBox]) -> (FieldWithConfidence, FieldWithConfidence) {
    const MID_X: f32 = 250.0; // 增值税发票标准版式的左右分栏中线
    const NAME_LABEL: &str = "名称：";

    let mut buyer = None;
    let mut seller = None;

    for label_box in boxes.iter().filter(|b| b.text.contains(NAME_LABEL)) {
        // 判定区块：x < 中线 → 买方，x >= 中线 → 卖方
        let is_buyer_side = label_box.x < MID_X;

        // 情况 1：标签和值在同一个框（"名称：赛比亚医疗诊断器械..."）
        if let Some(rest) = label_box.text.split(NAME_LABEL).nth(1) {
            let candidate = rest.trim();
            if !candidate.is_empty() {
                let result = Some((candidate.to_string(), label_box.confidence));
                if is_buyer_side {
                    buyer = result;
                } else {
                    seller = result;
                }
                continue;
            }
        }

        // 情况 2：值在同一行右侧**且在同一个区块内**的邻框
        let mut right_neighbors: Vec<&TextBox> = boxes
            .iter()
            .filter(|other| {
                (other.center_y() - label_box.center_y()).abs() <= SAME_LINE_TOLERANCE
                    && other.x >= label_box.right() - 5.0
                    && !std::ptr::eq(*other, label_box)
                    // 关键约束：右侧值必须在同一个区块内
                    && (is_buyer_side == (other.x < MID_X))
            })
            .collect();
        right_neighbors.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap());

        if let Some(value_box) = right_neighbors.first() {
            let name = value_box.text.trim().to_string();
            if !name.is_empty() {
                let result = Some((name, value_box.confidence));
                if is_buyer_side {
                    buyer = result;
                } else {
                    seller = result;
                }
            }
        }
    }

    (buyer, seller)
}

/// 从带坐标的票面中只读取销售方地址。传统增值税票的买卖双方都可能使用
/// “地址、电话”标签，因此无销售方语义时必须同时位于版面右半侧，不能把
/// 购买方注册地址误当作消费地。
fn find_seller_address_city(boxes: &[TextBox]) -> Option<String> {
    let min_x = boxes.iter().map(|text_box| text_box.x).reduce(f32::min)?;
    let max_x = boxes.iter().map(TextBox::right).reduce(f32::max)?;
    let middle_x = (min_x + max_x) / 2.0;
    let labels = [
        "销售方注册地址",
        "销售方地址",
        "销方地址",
        "注册地址",
        "地址、电话",
        "地址电话",
    ];

    for label in labels {
        for label_box in boxes
            .iter()
            .filter(|text_box| text_box.text.contains(label))
        {
            let has_seller_semantics = label_box.text.contains("销售方")
                || label_box.text.contains("销方")
                || label.starts_with("销售方")
                || label.starts_with("销方");
            if !has_seller_semantics && label_box.x < middle_x {
                continue;
            }

            if let Some(candidate) = label_box
                .text
                .split(label)
                .nth(1)
                .map(|value| value.trim_start_matches([' ', '：', ':', '\u{3000}']))
                .filter(|value| !value.is_empty())
            {
                if let Some(city) = field_extractor::extract_address_city(candidate) {
                    return Some(city);
                }
            }

            let mut candidates = boxes
                .iter()
                .filter(|candidate| {
                    !std::ptr::eq(*candidate, label_box)
                        && candidate.x >= middle_x
                        && ((candidate.center_y() - label_box.center_y()).abs()
                            <= SAME_LINE_TOLERANCE
                            || (candidate.y >= label_box.y
                                && candidate.y - label_box.y
                                    <= label_box.height.max(candidate.height) * 2.5))
                })
                .collect::<Vec<_>>();
            candidates.sort_by(|left, right| {
                let left_distance = (left.center_y() - label_box.center_y()).abs()
                    + (left.x - label_box.right()).abs();
                let right_distance = (right.center_y() - label_box.center_y()).abs()
                    + (right.x - label_box.right()).abs();
                left_distance.total_cmp(&right_distance)
            });
            if let Some(city) = candidates
                .into_iter()
                .find_map(|candidate| field_extractor::extract_address_city(&candidate.text))
            {
                return Some(city);
            }
        }
    }
    None
}

/// 税务/开票 App 的“开票成功 + 扫码下载”结果页会展示号码、日期和金额，
/// 但它只是领取入口，不是可报销的发票原件。必须同时命中状态锚点和操作锚点，
/// 避免因为真实发票备注里偶然出现单个“成功”词而误拒绝。
fn is_invoice_issuance_result_screen(boxes: &[TextBox]) -> bool {
    const STATUS_MARKERS: &[&str] = &["开具结果", "开票成功", "开具成功"];
    const ACTION_MARKERS: &[&str] = &["扫码下载发票", "继续开票"];

    let has_status = STATUS_MARKERS
        .iter()
        .any(|marker| boxes.iter().any(|text_box| text_box.text.contains(marker)));
    let has_action = ACTION_MARKERS
        .iter()
        .any(|marker| boxes.iter().any(|text_box| text_box.text.contains(marker)));
    has_status && has_action
}

/// 从 OCR 文本框中定位增值税发票字段。
///
/// 两种版式都要支持：
/// - 标签与值在同一个框内（"发票号码 12345"）
/// - 标签与值是相邻的两个框（"发票号码" | "12345"）
pub fn locate_vat_fields(
    boxes: &[TextBox],
    path: &Path,
    level: ParseLevel,
) -> Result<ParsedInvoice, ParseError> {
    if is_invoice_issuance_result_screen(boxes) {
        return Err(ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format: "发票图片",
            detail: "检测到开票成功/扫码下载结果页，不是发票原件".to_string(),
        });
    }

    let missing = |field: &str| ParseError::MissingField {
        path: path.to_path_buf(),
        field: field.to_string(),
    };

    let (number_raw, c1) = find_value(boxes, &["发票号码", "发票号"], looks_like_digits)
        .ok_or_else(|| missing("invoice_number"))?;
    let (date_raw, c2) = find_value(boxes, &["开票日期", "开具时间"], looks_like_date)
        .ok_or_else(|| missing("issue_date"))?;
    let (amount_raw, c3) = find_amount_value(boxes, &["价税合计", "合计金额", "小写", "合计"])
        .ok_or_else(|| missing("total_amount"))?;

    // 税额：优先行内定位，表格列版式作为兜底
    // 列定位器会按字符数跳过合并表头（如「税率/征收率税额」）。
    // 对于多行明细，先尝试求和逻辑，失败则取第一行
    let tax = find_value(boxes, &["税额", "税  额"], looks_like_amount)
        .or_else(|| {
            find_in_column_sum(boxes, &["税额", "税 额", "税  额"], looks_like_amount, 20.0)
        })
        .or_else(|| find_in_column(boxes, &["税额", "税 额", "税  额"], looks_like_amount, 20.0));
    let rate = find_value(boxes, &["税率"], looks_like_rate);

    // 购销方名称：完整语义标签不依赖版式，应当优先使用；
    // 只有传统左右分栏且没有完整标签时，才按「名称：」的坐标兜底。
    let (buyer_col, seller_col) = find_buyer_seller_names(boxes);
    // “购买方信息/销售方信息”只是区块标题，不能被宽泛的“购买方/销售方”
    // 标签截成值“信息”。完整语义标签失败时再使用左右栏的“名称：”字段。
    let buyer = find_value(boxes, &["购买方名称", "购方名称"], any_text).or(buyer_col);
    let seller = find_value(boxes, &["销售方名称", "销方名称"], any_text).or(seller_col);

    // 整张票的置信度取所有实际采用的框的最小值——
    // 一个字段错了，整张票就不能直接用
    let mut confidences = vec![c1, c2, c3];
    confidences.extend(
        [&tax, &rate]
            .iter()
            .filter_map(|o| o.as_ref().map(|(_, c)| *c)),
    );
    let confidence = confidences.iter().copied().fold(f32::INFINITY, f32::min);

    let seller_name = seller.as_ref().map(|(raw, _)| raw.clone());
    let issue_date = parse_date(&date_raw)?;
    let category_text = boxes
        .iter()
        .map(|text_box| text_box.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let ticket_type = crate::expense_classifier::classify_invoice_text(&category_text)
        .unwrap_or(TicketType::Other);

    Ok(ParsedInvoice {
        invoice_number: number_raw.chars().filter(|c| c.is_ascii_digit()).collect(),
        issue_date,
        total_amount: parse_amount(&amount_raw, "total_amount")?,
        tax_amount: tax
            .map(|(raw, _)| parse_amount(&raw, "tax_amount"))
            .transpose()?,
        tax_rate: rate.map(|(raw, _)| parse_tax_rate(&raw)).transpose()?,
        buyer_name: buyer.map(|(raw, _)| raw),
        seller_name: seller_name.clone(),
        ticket_type,
        transport_document_kind: field_extractor::extract_transport_document_kind(&category_text),
        parse_level: level,
        confidence,
        city: field_extractor::extract_city(&ticket_type, seller_name.as_deref().unwrap_or(""))
            .or_else(|| field_extractor::extract_seller_address_city(&category_text))
            .or_else(|| find_seller_address_city(boxes))
            .or_else(|| {
                field_extractor::extract_consistent_seller_jurisdiction_city(
                    &category_text,
                    seller_name.as_deref(),
                )
            }),
        travel_route: None,
        departure_time: None,
        checkin_date: None,
        source_path: path.to_path_buf(),
    })
}

/// 把同一行内水平相邻的碎片框合并成一个。
///
/// 有些 OFD 把一个词拆成多个 TextObject（"发"/"票号"/"码："），
/// 不合并就找不到 "发票号码" 这个标签。
/// `max_gap` 是允许的最大水平间隙（像素）：小于它就认为属于同一串文本。
pub fn merge_line_fragments(mut boxes: Vec<TextBox>, max_gap: f32) -> Vec<TextBox> {
    if boxes.is_empty() {
        return boxes;
    }
    // 先按行（y 中心）再按 x 排序
    boxes.sort_by(|a, b| {
        a.center_y()
            .partial_cmp(&b.center_y())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut out: Vec<TextBox> = Vec::with_capacity(boxes.len());
    for b in boxes {
        match out.last_mut() {
            Some(prev)
                if (prev.center_y() - b.center_y()).abs() <= SAME_LINE_TOLERANCE
                    && b.x - prev.right() <= max_gap
                    && b.x >= prev.x =>
            {
                // 同一行且紧邻：拼接文本，扩展边框
                prev.text.push_str(&b.text);
                let right = prev.right().max(b.right());
                prev.width = right - prev.x;
                prev.height = prev.height.max(b.height);
                prev.confidence = prev.confidence.min(b.confidence);
            }
            _ => out.push(b),
        }
    }
    out
}

/// 通过 Python sidecar 进行 OCR 识别。
///
/// 调用 tools/ocr_sidecar.py，返回文本框数组。
pub fn recognize_via_sidecar(image_path: &Path) -> anyhow::Result<Vec<TextBox>> {
    let project_root = std::env::current_dir()?;
    let sidecar_path = project_root.join("tools/ocr_sidecar.py");

    let output = std::process::Command::new("python3")
        .arg(sidecar_path)
        .arg(image_path)
        .output()?;

    if !output.status.success() {
        anyhow::bail!(
            "OCR sidecar failed with exit code: {:?}",
            output.status.code()
        );
    }

    let json = String::from_utf8(output.stdout)?;
    let boxes: Vec<TextBox> = serde_json::from_str(&json)?;
    Ok(boxes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromStr;
    use rust_decimal::Decimal;

    fn tb(text: &str, x: f32, y: f32, conf: f32) -> TextBox {
        TextBox {
            text: text.to_string(),
            x,
            y,
            width: text.chars().count() as f32 * 12.0,
            height: 20.0,
            confidence: conf,
        }
    }

    #[test]
    fn image_decode_uses_magic_when_extension_is_mismatched() {
        use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};
        use std::io::Cursor;
        use std::time::{SystemTime, UNIX_EPOCH};

        let image = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(8, 6, Rgb([1, 2, 3])));
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, ImageFormat::Png).unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "invoice-ocr-mismatched-extension-{}-{unique}.jpg",
            std::process::id()
        ));
        std::fs::write(&path, png.into_inner()).unwrap();

        let decoded = decode_image_with_guessed_format(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(decoded.width(), 8);
        assert_eq!(decoded.height(), 6);
    }

    /// 版式 A：标签和值在同一个框
    fn inline_layout() -> Vec<TextBox> {
        vec![
            tb("发票号码 24312000000012345678", 400.0, 40.0, 0.97),
            tb("开票日期 2026年07月03日", 400.0, 70.0, 0.95),
            tb("价税合计 ￥1280.00", 400.0, 300.0, 0.96),
            tb("税额 ￥72.45", 500.0, 260.0, 0.93),
            tb("税率 6%", 400.0, 260.0, 0.94),
        ]
    }

    /// 版式 B：标签和值是相邻的两个框（同一行）
    fn adjacent_layout() -> Vec<TextBox> {
        vec![
            tb("发票号码", 400.0, 40.0, 0.97),
            tb("24312000000012345678", 520.0, 42.0, 0.96),
            tb("开票日期", 400.0, 70.0, 0.95),
            tb("2026-07-03", 520.0, 71.0, 0.94),
            tb("价税合计", 400.0, 300.0, 0.96),
            tb("￥1280.00", 520.0, 301.0, 0.92),
        ]
    }

    #[test]
    fn locates_fields_in_inline_layout() {
        let invoice =
            locate_vat_fields(&inline_layout(), Path::new("a.jpg"), ParseLevel::L2).unwrap();
        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
        assert_eq!(invoice.total_amount, Decimal::from_str("1280.00").unwrap());
        assert_eq!(
            invoice.tax_amount,
            Some(Decimal::from_str("72.45").unwrap())
        );
        assert_eq!(invoice.tax_rate, Some(Decimal::from_str("0.06").unwrap()));
        assert_eq!(invoice.parse_level, ParseLevel::L2);
    }

    #[test]
    fn locates_fields_in_adjacent_layout() {
        let invoice =
            locate_vat_fields(&adjacent_layout(), Path::new("b.jpg"), ParseLevel::L2).unwrap();
        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
        assert_eq!(invoice.total_amount, Decimal::from_str("1280.00").unwrap());
    }

    #[test]
    fn rejects_invoice_issuance_result_screen_even_when_fields_are_present() {
        let mut boxes = inline_layout();
        boxes.extend([
            tb("开具结果", 400.0, 5.0, 0.99),
            tb("开票成功", 400.0, 100.0, 0.98),
            tb("扫码下载发票", 400.0, 150.0, 0.97),
            tb("继续开票", 400.0, 180.0, 0.96),
        ]);

        let error = locate_vat_fields(&boxes, Path::new("result-screen.png"), ParseLevel::L2)
            .expect_err("开票结果页不能作为发票原件进入草稿");
        assert!(matches!(error, ParseError::MalformedFormat { .. }));
        assert!(error.to_string().contains("不是发票原件"));
    }

    #[test]
    fn a_single_success_word_does_not_reject_an_invoice() {
        let mut boxes = inline_layout();
        boxes.push(tb("开票成功", 400.0, 180.0, 0.96));

        let invoice = locate_vat_fields(&boxes, Path::new("invoice.png"), ParseLevel::L2)
            .expect("单个状态词不足以判定为结果页");
        assert_eq!(invoice.invoice_number, "24312000000012345678");
    }

    #[test]
    fn confidence_is_minimum_across_used_boxes() {
        // 整张票的可信度由最弱的字段决定——一个字段错了整张就不能用
        let invoice =
            locate_vat_fields(&inline_layout(), Path::new("a.jpg"), ParseLevel::L2).unwrap();
        assert!(
            (invoice.confidence - 0.93).abs() < 0.001,
            "应取最低置信度 0.93，实际 {}",
            invoice.confidence
        );
    }

    #[test]
    fn ignores_label_box_on_a_different_line() {
        // "价税合计" 在 y=300，候选值在 y=500（不同行），不应被采用
        let boxes = vec![
            tb("发票号码", 400.0, 40.0, 0.9),
            tb("12345678901234567890", 520.0, 42.0, 0.9),
            tb("开票日期", 400.0, 70.0, 0.9),
            tb("2026-07-03", 520.0, 71.0, 0.9),
            tb("价税合计", 400.0, 300.0, 0.9),
            tb("￥999.00", 520.0, 500.0, 0.9),
        ];
        let err = locate_vat_fields(&boxes, Path::new("c.jpg"), ParseLevel::L2).unwrap_err();
        assert!(err.to_string().contains("total_amount"), "实际: {err}");
    }

    #[test]
    fn extracts_amount_from_chinese_uppercase_mixed_box() {
        // 同行右侧框 "壹拾伍圆整 ¥15.00"：中文大写 + 阿拉伯金额混合
        let boxes = vec![
            tb("发票号码", 400.0, 20.0, 0.97),
            tb("24312000000012345678", 520.0, 20.0, 0.96),
            tb("开票日期", 400.0, 40.0, 0.95),
            tb("2026-06-08", 520.0, 40.0, 0.94),
            tb("价税合计（大写）", 50.0, 200.0, 0.96),
            tb("壹拾伍圆整 ¥15.00", 250.0, 200.0, 0.93),
        ];
        let invoice = locate_vat_fields(&boxes, Path::new("test.pdf"), ParseLevel::L1).unwrap();
        assert_eq!(invoice.total_amount, Decimal::from_str("15.00").unwrap());
    }

    #[test]
    fn extracts_amount_from_next_line() {
        // 标签行无值，金额框在下方一行（y 差 15px < NEXT_LINE_TOLERANCE 30px）
        let boxes = vec![
            tb("发票号码", 400.0, 20.0, 0.97),
            tb("24312000000012345678", 520.0, 20.0, 0.96),
            tb("开票日期", 400.0, 40.0, 0.95),
            tb("2026-06-08", 520.0, 40.0, 0.94),
            // label: height=20 → bottom=220；value y=225 在 [215, 250] 范围内
            tb("（小写）", 50.0, 200.0, 0.96),
            tb("¥15.00", 120.0, 225.0, 0.93),
        ];
        let invoice = locate_vat_fields(&boxes, Path::new("test.pdf"), ParseLevel::L1).unwrap();
        assert_eq!(invoice.total_amount, Decimal::from_str("15.00").unwrap());
    }

    #[test]
    fn extracts_amount_from_overlapping_currency_and_number_boxes() {
        let boxes = vec![
            tb("发票号码 26132000001954318426", 50.0, 50.0, 1.0),
            tb("开票日期 2026年06月22日", 50.0, 100.0, 1.0),
            tb("￥", 420.0, 300.0, 1.0),
            tb("47.40", 420.0, 300.0, 1.0),
        ];
        let invoice = locate_vat_fields(&boxes, Path::new("overlap.ofd"), ParseLevel::L1)
            .expect("重叠币种符号应能锚定总金额");
        assert_eq!(invoice.total_amount, Decimal::from_str("47.40").unwrap());
    }

    #[test]
    fn extracts_lowest_currency_amount_without_a_text_label() {
        let boxes = vec![
            tb("发票号码 26132000001954318426", 50.0, 50.0, 1.0),
            tb("开票日期 2026年06月22日", 50.0, 100.0, 1.0),
            tb("￥3.20", 420.0, 250.0, 1.0),
            tb("￥47.40", 420.0, 300.0, 1.0),
        ];
        let invoice = locate_vat_fields(&boxes, Path::new("currency.ofd"), ParseLevel::L1)
            .expect("页面最下方币种金额应作为无标签总额兜底");
        assert_eq!(invoice.total_amount, Decimal::from_str("47.40").unwrap());
    }

    #[test]
    fn missing_invoice_number_reports_field() {
        let boxes = vec![
            tb("开票日期 2026-07-03", 400.0, 70.0, 0.9),
            tb("价税合计 ￥100.00", 400.0, 300.0, 0.9),
        ];
        let err = locate_vat_fields(&boxes, Path::new("d.jpg"), ParseLevel::L2).unwrap_err();
        assert!(err.to_string().contains("invoice_number"), "实际: {err}");
    }
    #[test]
    fn scaled_wide_tax_header_still_locates_tax_amount() {
        let mut tax_header = tb("税额", 220.0, 100.0, 0.99);
        tax_header.width = 128.0;
        let boxes = vec![
            tb("发票号码 26112000000000000001", 100.0, 20.0, 0.98),
            tb("开票日期 2026年06月18日", 100.0, 50.0, 0.97),
            tax_header,
            tb("67.92", 225.0, 150.0, 0.96),
            tb("价税合计：￥1200.00", 100.0, 250.0, 0.95),
        ];

        let invoice = locate_vat_fields(&boxes, Path::new("scaled.png"), ParseLevel::L2).unwrap();
        assert_eq!(
            invoice.tax_amount,
            Some(Decimal::from_str("67.92").unwrap())
        );
    }
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "由 scripts/verify-windows.ps1 显式执行离线 OCR 金样"]
    fn offline_ocr_reads_synthetic_vat_invoice() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let image_path = root.join("fixtures/synthetic/ocr-vat-invoice.png");
        let asset_dir = root.join("src-tauri/assets/ocr");

        let invoice = parse_invoice_image(&image_path, &asset_dir).unwrap();
        assert_eq!(invoice.invoice_number, "26112000000000000001");
        assert_eq!(invoice.issue_date.to_string(), "2026-06-18");
        assert_eq!(invoice.total_amount, Decimal::from_str("1200.00").unwrap());
        assert_eq!(
            invoice.tax_amount,
            Some(Decimal::from_str("67.92").unwrap())
        );
        assert_eq!(invoice.buyer_name.as_deref(), Some("北京示例科技有限公司"));
        assert_eq!(invoice.seller_name.as_deref(), Some("上海演示商贸有限公司"));
        assert_eq!(invoice.parse_level, ParseLevel::L2);
        assert!(
            invoice.confidence >= 0.85,
            "实际置信度 {}",
            invoice.confidence
        );
    }

    #[test]
    fn seller_address_city_uses_the_right_hand_invoice_column() {
        let boxes = vec![
            TextBox {
                text: "地址、电话".to_string(),
                x: 40.0,
                y: 120.0,
                width: 70.0,
                height: 14.0,
                confidence: 1.0,
            },
            TextBox {
                text: "北京市海淀区".to_string(),
                x: 115.0,
                y: 120.0,
                width: 100.0,
                height: 14.0,
                confidence: 1.0,
            },
            TextBox {
                text: "地址、电话".to_string(),
                x: 320.0,
                y: 120.0,
                width: 70.0,
                height: 14.0,
                confidence: 1.0,
            },
            TextBox {
                text: "上海市浦东新区".to_string(),
                x: 395.0,
                y: 120.0,
                width: 120.0,
                height: 14.0,
                confidence: 1.0,
            },
        ];

        assert_eq!(find_seller_address_city(&boxes).as_deref(), Some("上海"));
    }

    #[test]
    fn party_section_heading_is_not_mistaken_for_the_seller_name() {
        let boxes = vec![
            tb("发票号码：26112000000000000001", 20.0, 10.0, 1.0),
            tb("开票日期：2026-06-01", 20.0, 35.0, 1.0),
            tb("价税合计：￥646.70", 20.0, 60.0, 1.0),
            tb("国家税务总局上海市税务局", 300.0, 85.0, 1.0),
            tb("销售方信息", 300.0, 110.0, 1.0),
            tb("名称：示例餐饮管理（上海）有限公司", 300.0, 135.0, 1.0),
        ];

        let invoice = locate_vat_fields(&boxes, Path::new("invoice.png"), ParseLevel::L2).unwrap();

        assert_eq!(
            invoice.seller_name.as_deref(),
            Some("示例餐饮管理（上海）有限公司")
        );
        assert_eq!(invoice.city.as_deref(), Some("上海"));
    }
}
