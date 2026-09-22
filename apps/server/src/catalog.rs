use serde::{Deserialize, Serialize};

pub const CATALOG_JSON: &str = include_str!("../catalog/v1.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalog {
    pub schema_version: u32,
    pub catalog_version: String,
    pub entries: Vec<CatalogEntry>,
    pub profiles: Vec<SetupProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub roles: Vec<String>,
    pub provider: String,
    pub repository: String,
    pub revision: String,
    pub artifact: String,
    pub download_url: String,
    pub format: String,
    pub quantization: String,
    pub bytes: u64,
    pub sha256: String,
    pub license: String,
    pub license_url: String,
    pub access: String,
    pub runtime: String,
    pub runtime_status: RuntimeStatus,
    pub context_limit: u32,
    pub tokenizer: String,
    pub chat_template: Option<String>,
    pub capabilities: Vec<String>,
    pub estimated_ram_bytes: u64,
    pub estimated_vram_bytes: Option<u64>,
    pub disk_overhead_bytes: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStatus {
    Executable,
    CatalogOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupProfile {
    pub id: String,
    pub name: String,
    pub entry_ids: Vec<String>,
}

impl ModelCatalog {
    /// Parses the embedded, reviewable catalog.
    ///
    /// # Errors
    ///
    /// Returns an error if the checked-in catalog is malformed.
    pub fn embedded() -> Result<Self, serde_json::Error> {
        serde_json::from_str(CATALOG_JSON)
    }

    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&CatalogEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_pinned_and_covers_every_phase_two_role() {
        let catalog = ModelCatalog::embedded().unwrap();
        for role in [
            "minimal_text_chat",
            "recommended_text_chat",
            "vision",
            "embedding_utility",
            "ocr_utility",
            "transcription_utility",
            "safety_optional",
        ] {
            assert!(
                catalog
                    .entries
                    .iter()
                    .any(|entry| entry.roles.iter().any(|candidate| candidate == role)),
                "missing catalog role {role}"
            );
        }
        assert!(catalog.entries.iter().all(|entry| {
            entry.revision.len() == 40
                && entry.sha256.len() == 64
                && !entry.download_url.contains("/main/")
        }));
    }
}
