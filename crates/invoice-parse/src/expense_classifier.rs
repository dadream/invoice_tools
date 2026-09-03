//! Conservative expense-category suggestions derived from invoice content.
//!
//! This module deliberately uses service/item anchors rather than seller names. A seller may
//! operate several businesses, while the invoice line item is the auditable reason for a
//! category suggestion. No match means "unclassified" and must remain user-reviewable.

use crate::model::TicketType;

/// Return a high-confidence expense category when the invoice content contains a strong service
/// anchor. The caller keeps `Other` when this function returns `None`.
pub fn classify_invoice_text(text: &str) -> Option<TicketType> {
    let compact = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_lowercase();

    if contains_any(
        &compact,
        &[
            "网约车服务",
            "出租汽车客运服务",
            "出租车客运服务",
            "客运服务费",
            "代驾服务",
            "城市客运服务",
        ],
    ) || is_split_passenger_service_fee(&compact)
    {
        return Some(TicketType::CityTransport);
    }
    if contains_any(&compact, &["住宿服务", "住宿费", "客房服务费", "酒店住宿"]) {
        return Some(TicketType::Hotel);
    }
    if contains_any(
        &compact,
        &["餐饮服务", "餐饮费", "餐费", "餐饮服务费", "食品餐饮服务"],
    ) || is_railway_onboard_food(&compact)
    {
        return Some(TicketType::Meal);
    }
    if contains_any(
        &compact,
        &[
            "收派服务",
            "快递服务",
            "快递费",
            "物流服务",
            "物流费",
            "同城配送服务",
            "配送服务费",
            "配送服务",
        ],
    ) {
        return Some(TicketType::CourierLogistics);
    }
    None
}

/// `Other` 表示调用方没有提供明确类型提示，必须保留解析器从票面识别出的类型。
/// 只有用户或上游明确指定了非 `Other` 类型时，才覆盖自动识别结果。
pub fn resolve_ticket_type_hint(detected: TicketType, hint: TicketType) -> TicketType {
    if hint == TicketType::Other {
        detected
    } else {
        hint
    }
}

/// Return a conservative category suggestion from a legal merchant name.
///
/// This is intentionally separate from `classify_invoice_text`: invoice item/service content is
/// auditable and can be accepted automatically, while a merchant-name match remains a suggestion
/// that the product must show as unconfirmed. Only explicit business-format words are used here;
/// generic words such as “食品”, “商贸” and “管理” are deliberately excluded.
pub fn classify_merchant_name(name: &str) -> Option<TicketType> {
    let compact = name
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_lowercase();

    if contains_any(
        &compact,
        &[
            "滴滴出行",
            "滴滴畅行",
            "首汽约车",
            "曹操出行",
            "享道出行",
            "出租汽车",
            "出租车",
            "网约车",
        ],
    ) {
        return Some(TicketType::CityTransport);
    }
    if contains_any(&compact, &["酒店", "宾馆", "旅馆", "客栈", "住宿"]) {
        return Some(TicketType::Hotel);
    }
    if contains_any(
        &compact,
        &[
            "餐饮",
            "餐厅",
            "饭店",
            "饭馆",
            "火锅店",
            "咖啡",
            "牛肉粉馆",
            "烧烤",
            "酒楼",
            "食府",
            "快餐",
            "小吃店",
            "面馆",
            "饺子馆",
        ],
    ) {
        return Some(TicketType::Meal);
    }
    None
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// 铁路列车服务商开具的方便食品/饮料属于旅途中实际餐饮。只有“铁路/列车
/// 服务商 + 食品项目”两个证据同时出现才命中，普通零食、商超和健身服装仍
/// 保持未分类。
fn is_railway_onboard_food(compact: &str) -> bool {
    let railway_vendor = contains_any(
        compact,
        &[
            "铁路文化旅游有限公司",
            "京铁列车服务有限公司",
            "列车服务有限公司",
            "铁路餐饮",
            "列车餐饮",
        ],
    );
    let food_item = contains_any(
        compact,
        &[
            "方便食品",
            "软饮料",
            "果类加工品",
            "豆制品",
            "熟肉制品",
            "餐食",
            "盒饭",
        ],
    );
    railway_vendor && food_item
}

/// 部分滴滴数电票的 PDF 文本层把项目名拆成：
/// `*交通运输服务*客运服 <金额/数量/税率列> 务费`。普通空白归一化无法
/// 恢复这个被表格列穿插的词，因此用三组强锚点识别；不接受单独的“客运服务”，
/// 避免把长途客运票误判为市内交通。
fn is_split_passenger_service_fee(compact: &str) -> bool {
    compact.contains("旅客运输服务")
        && compact.contains("交通运输服务")
        && compact.contains("客运服")
        && compact.contains("务费")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_strong_invoice_item_anchors() {
        assert_eq!(
            classify_invoice_text("项目名称 *运输服务*网约车服务费"),
            Some(TicketType::CityTransport)
        );
        assert_eq!(
            classify_invoice_text("货物或应税劳务名称：住宿服务"),
            Some(TicketType::Hotel)
        );
        assert_eq!(
            classify_invoice_text("项目名称 *餐饮服务*餐费"),
            Some(TicketType::Meal)
        );
        assert_eq!(
            classify_invoice_text("项目名称 *生产生活服务*收派服务费"),
            Some(TicketType::CourierLogistics)
        );
        assert_eq!(
            classify_invoice_text("项目名称 *生产生活服务*配送服务费"),
            Some(TicketType::CourierLogistics)
        );
        assert_eq!(
            classify_invoice_text(
                "旅客运输服务\n项目名称 *交通运输服务*客运服 19.48 1 19.48 3% 0.58\n务费"
            ),
            Some(TicketType::CityTransport)
        );
    }

    #[test]
    fn keeps_out_of_scope_retail_and_personal_items_unclassified() {
        assert_eq!(classify_invoice_text("项目名称 *方便食品*午餐套餐"), None);
        assert_eq!(classify_invoice_text("项目名称 *其他食品*其他食品"), None);
        assert_eq!(
            classify_invoice_text("项目名称 *生产生活服务*体育健身服务"),
            None
        );
        assert_eq!(
            classify_invoice_text("项目名称 *服装*运动服装 *鞋*运动鞋"),
            None
        );
        assert_eq!(classify_invoice_text("公路旅客运输服务 客运服务"), None);
    }

    #[test]
    fn classifies_railway_onboard_food_without_classifying_generic_snacks() {
        assert_eq!(
            classify_invoice_text(
                "销售方：北京京铁列车服务有限公司石家庄分公司 项目：*方便食品*杏鲍菇烧牛肉"
            ),
            Some(TicketType::Meal)
        );
        assert_eq!(
            classify_invoice_text(
                "销售方：山西铁路文化旅游有限公司唐盛源分公司 *方便食品*方便食品"
            ),
            Some(TicketType::Meal)
        );
        assert_eq!(
            classify_invoice_text("销售方：某商贸有限公司 *方便食品*薯片"),
            None
        );
    }

    #[test]
    fn other_hint_preserves_detected_type() {
        assert_eq!(
            resolve_ticket_type_hint(TicketType::Meal, TicketType::Other),
            TicketType::Meal
        );
        assert_eq!(
            resolve_ticket_type_hint(TicketType::Hotel, TicketType::Other),
            TicketType::Hotel
        );
        assert_eq!(
            resolve_ticket_type_hint(TicketType::Meal, TicketType::Hotel),
            TicketType::Hotel
        );
    }

    #[test]
    fn seller_name_alone_does_not_force_a_category() {
        assert_eq!(
            classify_invoice_text("销售方名称：北京某某餐饮管理有限公司"),
            None
        );
        assert_eq!(
            classify_invoice_text("销售方名称：某某出行科技有限公司"),
            None
        );
    }

    #[test]
    fn explicit_merchant_business_words_produce_reviewable_suggestions() {
        assert_eq!(
            classify_merchant_name("太原市迎泽区顺华牛肉粉馆水西门店"),
            Some(TicketType::Meal)
        );
        assert_eq!(
            classify_merchant_name("北京喜小满餐饮管理有限公司"),
            Some(TicketType::Meal)
        );
        assert_eq!(
            classify_merchant_name("北京滴滴出行科技有限公司"),
            Some(TicketType::CityTransport)
        );
        assert_eq!(
            classify_merchant_name("河北顺德城市运营管理有限公司邢台假日酒店分公司"),
            Some(TicketType::Hotel)
        );
        assert_eq!(classify_merchant_name("北京华夏龙源商贸有限公司"), None);
        assert_eq!(classify_merchant_name("北京某某食品有限公司"), None);
    }
}
