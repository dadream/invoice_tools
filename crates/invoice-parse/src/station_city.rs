//! 离线铁路车站到城市的确定性解析。
//!
//! 车站名称不一定包含所属城市（例如“清河”“汉口”），不能仅通过删除“站”字推断。
//! MVP 只将用户常驻城市的站点别名随程序发布；未命中时由上层执行保守规则并保留审核。

use once_cell::sync::Lazy;
use serde::Deserialize;
use std::collections::HashMap;

const DATASET_JSON: &str = include_str!("../assets/rail-station-city.v1.json");

#[derive(Debug, Deserialize)]
struct StationCityDataset {
    schema_version: u32,
    dataset_version: String,
    scope: String,
    stations: Vec<StationCityRecord>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct StationCityRecord {
    pub station_name: String,
    pub city_name: String,
    pub province_name: String,
    pub aliases: Vec<String>,
    pub source_url: String,
}

static DATASET: Lazy<StationCityDataset> = Lazy::new(|| {
    let dataset: StationCityDataset = serde_json::from_str(DATASET_JSON)
        .expect("bundled rail station city dataset must be valid");
    assert_eq!(
        dataset.schema_version, 1,
        "unsupported bundled rail station city dataset schema"
    );
    dataset
});

static STATION_INDEX: Lazy<HashMap<String, Vec<usize>>> = Lazy::new(|| {
    let mut index = HashMap::new();
    for (record_index, record) in DATASET.stations.iter().enumerate() {
        for name in std::iter::once(&record.station_name).chain(record.aliases.iter()) {
            let key = normalize_station_key(name);
            index.entry(key).or_insert_with(Vec::new).push(record_index);
        }
    }
    index
});

fn normalize_station_key(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn normalize_city_key(value: &str) -> String {
    value.trim().trim_end_matches(['市', '省']).to_string()
}

/// 在用户配置的常驻城市范围内精确解析车站名称或别名。
///
/// “朝阳站”等名称在全国范围可能有歧义，只有常驻城市包含北京时才会命中北京站点库。
/// 模糊匹配不得进入此函数。
pub fn resolve_home_city_station(
    station_name: &str,
    home_cities: &[String],
) -> Option<&'static StationCityRecord> {
    let key = normalize_station_key(station_name);
    STATION_INDEX
        .get(&key)
        .into_iter()
        .flatten()
        .filter_map(|record_index| DATASET.stations.get(*record_index))
        .find(|record| {
            let record_city = normalize_city_key(&record.city_name);
            home_cities
                .iter()
                .any(|home_city| normalize_city_key(home_city) == record_city)
        })
}

pub fn station_city_dataset_version() -> &'static str {
    &DATASET.dataset_version
}

pub fn station_city_dataset_scope() -> &'static str {
    &DATASET.scope
}

/// 返回某个常驻城市随程序发布的默认车站记录，供设置界面初始化可编辑副本。
pub fn station_city_records_for_city(city_name: &str) -> Vec<StationCityRecord> {
    let city_key = normalize_city_key(city_name);
    DATASET
        .stations
        .iter()
        .filter(|record| normalize_city_key(&record.city_name) == city_key)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_non_city_named_station_by_exact_alias() {
        let match_record = resolve_home_city_station(" 清河站 ", &["北京".to_string()])
            .expect("常驻城市为北京时清河站应命中离线站点表");
        assert_eq!(match_record.station_name, "清河");
        assert_eq!(match_record.city_name, "北京");
        assert_eq!(match_record.province_name, "北京");
        assert!(match_record.source_url.starts_with("https://"));
    }

    #[test]
    fn resolves_station_aliases_without_fuzzy_guessing() {
        assert_eq!(
            resolve_home_city_station("朝阳站", &["北京".to_string()])
                .map(|record| record.city_name.as_str()),
            Some("北京")
        );
        assert!(resolve_home_city_station("朝阳站", &["辽宁".to_string()]).is_none());
        assert!(resolve_home_city_station("清河城", &["北京".to_string()]).is_none());
    }

    #[test]
    fn bundled_dataset_has_explicit_version_and_scope() {
        assert_eq!(DATASET.schema_version, 1);
        assert!(!station_city_dataset_version().trim().is_empty());
        assert!(station_city_dataset_scope().contains("MVP"));
    }
}
