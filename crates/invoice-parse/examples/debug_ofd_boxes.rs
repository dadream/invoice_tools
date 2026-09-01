use invoice_parse::ofd_text::extract_text_boxes;
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("用法: debug_ofd_boxes <file.ofd>");

    let bytes = std::fs::read(path)?;
    let boxes = extract_text_boxes(&bytes, Path::new(path))?;

    println!("提取到 {} 个文本框（原始）:\n", boxes.len());
    for (i, b) in boxes.iter().enumerate() {
        println!(
            "{:3}: ({:6.1}, {:6.1}) {}x{} conf={:.2} \"{}\"",
            i, b.x, b.y, b.width as i32, b.height as i32, b.confidence, b.text
        );
    }

    // 合并后
    let merged = invoice_parse::ocr::merge_line_fragments(boxes, 6.0);
    println!("\n合并后 {} 个文本框:\n", merged.len());
    for (i, b) in merged.iter().enumerate() {
        println!(
            "{:3}: ({:6.1}, {:6.1}) {}x{} \"{}\"",
            i, b.x, b.y, b.width as i32, b.height as i32, b.text
        );
    }

    Ok(())
}
