use crate::model::ParseError;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::path::PathBuf;

/// XML 中一个含文本的叶子元素。
#[derive(Debug, Clone, PartialEq)]
pub struct LeafElement {
    pub tag: String,
    pub text: String,
    pub depth: usize,
}

/// 遍历 XML，收集所有含非空文本的叶子元素。
/// 命名空间前缀会被剥离（`tax:TotalAmount` → `TotalAmount`），
/// 因为不同平台的前缀不同但本地名通常一致。
pub fn collect_leaf_elements(xml_bytes: &[u8]) -> Result<Vec<LeafElement>, ParseError> {
    let mut reader = Reader::from_reader(xml_bytes);
    reader.config_mut().trim_text(true);

    let mut leaves = Vec::new();
    let mut buf = Vec::new();
    // 栈顶记录当前元素的 (标签名, 深度, 是否已见过子元素)
    let mut stack: Vec<(String, usize, bool)> = Vec::new();
    let mut pending_text: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if let Some(parent) = stack.last_mut() {
                    parent.2 = true;
                }
                let tag = local_name(e.name().as_ref());
                let depth = stack.len();
                stack.push((tag, depth, false));
                pending_text = None;
            }
            Ok(Event::Text(e)) => {
                let text = e
                    .unescape()
                    .map_err(|err| ParseError::MalformedFormat {
                        path: PathBuf::new(),
                        format: "XML",
                        detail: format!("文本节点解码失败: {err}"),
                    })?
                    .trim()
                    .to_string();
                if !text.is_empty() {
                    pending_text = Some(text);
                }
            }
            Ok(Event::End(_)) => {
                if let Some((tag, depth, had_children)) = stack.pop() {
                    if !had_children {
                        if let Some(text) = pending_text.take() {
                            leaves.push(LeafElement { tag, text, depth });
                        }
                    }
                }
                pending_text = None;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(ParseError::MalformedFormat {
                    path: PathBuf::new(),
                    format: "XML",
                    detail: err.to_string(),
                })
            }
        }
        buf.clear();
    }

    if !stack.is_empty() {
        return Err(ParseError::MalformedFormat {
            path: PathBuf::new(),
            format: "XML",
            detail: format!("有 {} 个元素未闭合", stack.len()),
        });
    }

    Ok(leaves)
}

/// 剥离命名空间前缀：`tax:Number` → `Number`
fn local_name(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    match s.rsplit_once(':') {
        Some((_, local)) => local.to_string(),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_nested_leaf_text() {
        let xml = br#"<Invoice>
            <Header><Number>12345</Number></Header>
            <Body><Amount>553.00</Amount><Tax>50.73</Tax></Body>
        </Invoice>"#;

        let leaves = collect_leaf_elements(xml).unwrap();

        assert_eq!(leaves.len(), 3);
        assert_eq!(leaves[0], LeafElement { tag: "Number".into(), text: "12345".into(), depth: 2 });
        assert_eq!(leaves[1], LeafElement { tag: "Amount".into(), text: "553.00".into(), depth: 2 });
        assert_eq!(leaves[2], LeafElement { tag: "Tax".into(), text: "50.73".into(), depth: 2 });
    }

    #[test]
    fn strips_namespace_prefix() {
        let xml = br#"<tax:Invoice xmlns:tax="urn:x"><tax:Number>999</tax:Number></tax:Invoice>"#;
        let leaves = collect_leaf_elements(xml).unwrap();
        assert_eq!(leaves[0].tag, "Number");
    }

    #[test]
    fn skips_whitespace_only_elements() {
        let xml = br#"<Root><Empty>   </Empty><Real>x</Real></Root>"#;
        let leaves = collect_leaf_elements(xml).unwrap();
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].tag, "Real");
    }

    #[test]
    fn trims_surrounding_whitespace_in_text() {
        let xml = "<Root><Name>  某某公司
  </Name></Root>".as_bytes();
        let leaves = collect_leaf_elements(xml).unwrap();
        assert_eq!(leaves[0].text, "某某公司");
    }

    #[test]
    fn malformed_xml_returns_error() {
        let xml = br#"<Root><Unclosed></Root>"#;
        assert!(collect_leaf_elements(xml).is_err());
    }
}
