use invoice_grouping::types::*;

/// 测试用 Mock 解决器，总是选择第一个候选方案
pub struct AlwaysFirstResolver;

impl AmbiguityResolver for AlwaysFirstResolver {
    fn resolve(
        &self,
        ambiguities: &[Ambiguity],
    ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
        Ok(ambiguities
            .iter()
            .enumerate()
            .map(|(idx, amb)| AmbiguityResolution {
                ambiguity_index: idx,
                chosen_candidate: 0,
                confidence: 0.8,
                reason: format!("Mock: 选择第一个候选 - {}", amb.candidates[0]),
            })
            .collect())
    }
}

/// 测试用 Mock 解决器，总是选择最后一个候选方案
pub struct AlwaysLastResolver;

impl AmbiguityResolver for AlwaysLastResolver {
    fn resolve(
        &self,
        ambiguities: &[Ambiguity],
    ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
        Ok(ambiguities
            .iter()
            .enumerate()
            .map(|(idx, amb)| AmbiguityResolution {
                ambiguity_index: idx,
                chosen_candidate: amb.candidates.len().saturating_sub(1),
                confidence: 0.8,
                reason: format!(
                    "Mock: 选择最后一个候选 - {}",
                    amb.candidates.last().unwrap_or(&"".to_string())
                ),
            })
            .collect())
    }
}
