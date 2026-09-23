use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    catalog::{CatalogEntry, RuntimeStatus},
    hardware::HardwareCapabilities,
    models::ModelError,
};

const HF_API: &str = "https://huggingface.co/api/models";

#[derive(Debug, Clone, Serialize)]
pub struct ModelSearchResponse {
    pub provider: &'static str,
    pub query: String,
    pub normalized_query: String,
    pub candidates: Vec<ModelCandidate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelCandidate {
    pub candidate_id: String,
    pub provider: String,
    pub repository: String,
    pub revision: String,
    pub artifact: String,
    pub display_name: String,
    pub parameter_size: Option<String>,
    pub quantization: Option<String>,
    pub format: String,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
    pub license: Option<String>,
    pub access: String,
    pub context_limit: Option<u32>,
    pub tokenizer: Option<String>,
    pub chat_template_available: Option<bool>,
    pub capabilities: Vec<String>,
    pub runtime_compatible: bool,
    pub runtime_reason: String,
    pub estimated_disk_bytes: Option<u64>,
    pub estimated_ram_bytes: Option<u64>,
    pub downloadable: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HfModel {
    #[serde(rename = "id")]
    id: String,
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    gated: serde_json::Value,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    siblings: Vec<HfFile>,
    #[serde(default)]
    gguf: Option<HfGguf>,
}

#[derive(Debug, Clone, Deserialize)]
struct HfFile {
    rfilename: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    lfs: Option<HfLfs>,
}

#[derive(Debug, Clone, Deserialize)]
struct HfLfs {
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct HfGguf {
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    chat_template: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactSelection {
    pub provider: String,
    pub repository: String,
    pub revision: String,
    pub artifact: String,
}

/// Searches the supported provider and returns bounded, immutable GGUF candidates.
///
/// # Errors
///
/// Returns an error for invalid queries, provider failures, or malformed provider data.
pub async fn search(
    client: &Client,
    query: &str,
    hardware: &HardwareCapabilities,
) -> Result<ModelSearchResponse, ModelError> {
    let query = query.trim();
    if query.is_empty() || query.len() > 200 {
        return Err(ModelError::Invalid(
            "model search must contain 1 to 200 characters".to_owned(),
        ));
    }
    let normalized = normalize_query(query);
    let summaries = if exact_repository(query) {
        vec![fetch_model(client, query).await?]
    } else {
        let mut url =
            Url::parse(HF_API).map_err(|error| ModelError::Provider(error.to_string()))?;
        url.query_pairs_mut()
            .append_pair("search", &normalized)
            .append_pair("filter", "gguf")
            .append_pair("sort", "downloads")
            .append_pair("direction", "-1")
            .append_pair("limit", "5");
        let response = client.get(url).send().await?.error_for_status()?;
        response.json::<Vec<HfModel>>().await?
    };
    let mut candidates = Vec::new();
    for summary in summaries {
        let model = if summary.siblings.iter().any(|file| file.size.is_some()) {
            summary
        } else {
            fetch_model(client, &summary.id).await?
        };
        candidates.extend(candidates_for(&model, hardware));
    }
    candidates.sort_by(|left, right| {
        left.repository
            .cmp(&right.repository)
            .then(left.bytes.cmp(&right.bytes))
            .then(left.artifact.cmp(&right.artifact))
    });
    Ok(ModelSearchResponse {
        provider: "hugging_face",
        query: query.to_owned(),
        normalized_query: normalized,
        candidates,
    })
}

/// Resolves an exact provider selection again immediately before job creation.
///
/// # Errors
///
/// Returns an error if the selection is invalid, moved, gated, or lacks verification metadata.
pub async fn resolve(
    client: &Client,
    selection: &ArtifactSelection,
    hardware: &HardwareCapabilities,
) -> Result<CatalogEntry, ModelError> {
    validate_selection(selection)?;
    let model = fetch_model(client, &selection.repository).await?;
    if model.sha.as_deref() != Some(selection.revision.as_str()) {
        return Err(ModelError::Invalid(
            "the selected immutable revision is no longer the repository revision; search again"
                .to_owned(),
        ));
    }
    let candidate = candidates_for(&model, hardware)
        .into_iter()
        .find(|candidate| candidate.artifact == selection.artifact)
        .ok_or_else(|| {
            ModelError::Invalid(
                "the selected artifact is not a compatible single-file GGUF".to_owned(),
            )
        })?;
    if !candidate.downloadable {
        return Err(ModelError::Invalid(
            candidate.unavailable_reason.unwrap_or_else(|| {
                "the selected artifact cannot be downloaded without credentials".to_owned()
            }),
        ));
    }
    let bytes = candidate
        .bytes
        .ok_or_else(|| ModelError::Invalid("artifact size is unavailable".to_owned()))?;
    let sha256 = candidate
        .sha256
        .ok_or_else(|| ModelError::Invalid("artifact SHA-256 is unavailable".to_owned()))?;
    let download_url = download_url(
        &candidate.repository,
        &candidate.revision,
        &candidate.artifact,
    )?;
    Ok(CatalogEntry {
        id: candidate.candidate_id,
        name: candidate.display_name,
        roles: vec!["custom_text_chat".to_owned()],
        provider: "hugging_face".to_owned(),
        repository: candidate.repository,
        revision: candidate.revision,
        artifact: candidate.artifact,
        download_url,
        format: "gguf".to_owned(),
        quantization: candidate
            .quantization
            .unwrap_or_else(|| "Unknown".to_owned()),
        bytes,
        sha256,
        license: candidate.license.unwrap_or_else(|| "Unknown".to_owned()),
        license_url: "https://huggingface.co/docs/hub/repositories-licenses".to_owned(),
        access: candidate.access,
        runtime: "llama_cpp".to_owned(),
        runtime_status: RuntimeStatus::Executable,
        context_limit: candidate.context_limit.unwrap_or(0),
        tokenizer: candidate.tokenizer.unwrap_or_else(|| "Unknown".to_owned()),
        chat_template: candidate.chat_template_available.map(|available| {
            if available {
                "available"
            } else {
                "not reported"
            }
            .to_owned()
        }),
        capabilities: candidate.capabilities,
        estimated_ram_bytes: candidate.estimated_ram_bytes.unwrap_or(bytes),
        estimated_vram_bytes: None,
        disk_overhead_bytes: 64 * 1024 * 1024,
        reason: "User-selected immutable Hugging Face GGUF artifact.".to_owned(),
    })
}

async fn fetch_model(client: &Client, repository: &str) -> Result<HfModel, ModelError> {
    if !valid_repository(repository) {
        return Err(ModelError::Invalid(
            "repository must be a Hugging Face owner/name identifier".to_owned(),
        ));
    }
    let mut url = Url::parse(HF_API).map_err(|error| ModelError::Provider(error.to_string()))?;
    for segment in repository.split('/') {
        url.path_segments_mut()
            .map_err(|()| ModelError::Provider("invalid provider API URL".to_owned()))?
            .push(segment);
    }
    url.query_pairs_mut().append_pair("blobs", "true");
    let response = client.get(url).send().await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        || response.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Err(ModelError::Invalid(
            "this repository requires Hugging Face credentials; Tome does not store provider tokens"
                .to_owned(),
        ));
    }
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(ModelError::Invalid(
            "no Hugging Face model repository matched that identifier".to_owned(),
        ));
    }
    Ok(response.error_for_status()?.json().await?)
}

fn candidates_for(model: &HfModel, hardware: &HardwareCapabilities) -> Vec<ModelCandidate> {
    let revision = model.sha.clone().unwrap_or_default();
    let access_limited = model.private || gated(&model.gated);
    let license = model
        .tags
        .iter()
        .find_map(|tag| tag.strip_prefix("license:"))
        .map(str::to_owned);
    let parameter_size = parameter_size(&model.id);
    model
        .siblings
        .iter()
        .filter(|file| is_single_gguf(&file.rfilename))
        .map(|file| {
            let bytes = file.size.or_else(|| file.lfs.as_ref().and_then(|lfs| lfs.size));
            let sha256 = file.lfs.as_ref().and_then(|lfs| lfs.sha256.clone());
            let quantization = quantization(&file.rfilename);
            let estimated_ram_bytes = bytes.map(|value| {
                value.saturating_add((value / 5).max(512 * 1024 * 1024))
            });
            let runtime_compatible = hardware.runtime.llama_cpp.available
                && estimated_ram_bytes.is_none_or(|ram| ram <= hardware.total_memory_bytes);
            let runtime_reason = if !hardware.runtime.llama_cpp.available {
                hardware.runtime.llama_cpp.reason.clone()
            } else if estimated_ram_bytes.is_some_and(|ram| ram > hardware.total_memory_bytes) {
                "Estimated RAM exceeds detected system RAM.".to_owned()
            } else {
                "Single-file GGUF supported by the detected llama.cpp runtime.".to_owned()
            };
            let metadata_complete = valid_hex(&revision, 40)
                && bytes.is_some_and(|value| value > 0)
                && sha256
                    .as_deref()
                    .is_some_and(|value| valid_hex(value, 64));
            let downloadable = !access_limited && metadata_complete;
            let unavailable_reason = if access_limited {
                Some("Credential-gated/private repositories are unsupported because Tome does not store provider tokens.".to_owned())
            } else if !metadata_complete {
                Some("The provider did not report an immutable revision, exact size, and SHA-256 for this artifact.".to_owned())
            } else {
                None
            };
            let identity = format!("{}@{}:{}", model.id, revision, file.rfilename);
            let digest = format!("{:x}", Sha256::digest(identity.as_bytes()));
            ModelCandidate {
                candidate_id: format!("hf-{}", &digest[..24]),
                provider: "hugging_face".to_owned(),
                repository: model.id.clone(),
                revision: revision.clone(),
                artifact: file.rfilename.clone(),
                display_name: format!("{} · {}", model.id, quantization.as_deref().unwrap_or("GGUF")),
                parameter_size: parameter_size.clone(),
                quantization,
                format: "gguf".to_owned(),
                bytes,
                sha256,
                license: license.clone(),
                access: if access_limited { "credential_required" } else { "public" }.to_owned(),
                context_limit: model.gguf.as_ref().and_then(|gguf| gguf.context_length),
                tokenizer: None,
                chat_template_available: model.gguf.as_ref().map(|gguf| gguf.chat_template.is_some()),
                capabilities: vec!["text_input".to_owned(), "text_output".to_owned()],
                runtime_compatible,
                runtime_reason,
                estimated_disk_bytes: bytes.map(|value| value.saturating_add(64 * 1024 * 1024)),
                estimated_ram_bytes,
                downloadable,
                unavailable_reason,
            }
        })
        .collect()
}

fn normalize_query(query: &str) -> String {
    let lower = query.to_ascii_lowercase();
    if let Some((family, size)) = lower.split_once(':') {
        let family = family.replace("llama3.2", "Llama 3.2").replace('-', " ");
        return format!("{} {} GGUF", family, size.to_ascii_uppercase());
    }
    if exact_repository(query) || lower.contains("gguf") {
        query.to_owned()
    } else {
        format!("{query} GGUF")
    }
}

fn validate_selection(selection: &ArtifactSelection) -> Result<(), ModelError> {
    if selection.provider != "hugging_face" {
        return Err(ModelError::Invalid(
            "only the hugging_face provider is supported".to_owned(),
        ));
    }
    if !valid_repository(&selection.repository)
        || selection.revision.len() != 40
        || !selection
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !is_single_gguf(&selection.artifact)
    {
        return Err(ModelError::Invalid(
            "invalid immutable Hugging Face artifact selection".to_owned(),
        ));
    }
    Ok(())
}

fn valid_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if valid_slug(a) && valid_slug(b))
}

fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn exact_repository(value: &str) -> bool {
    valid_repository(value)
}

fn is_single_gguf(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    std::path::Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
        && !lower.contains("-00001-of-")
        && !name.contains(['/', '\\'])
}

fn gated(value: &serde_json::Value) -> bool {
    value.as_bool().unwrap_or(false)
        || value
            .as_str()
            .is_some_and(|state| !matches!(state, "false" | "none"))
}

fn quantization(name: &str) -> Option<String> {
    let upper = name.to_ascii_uppercase();
    [
        "IQ2_XXS", "IQ3_XXS", "IQ1_S", "IQ1_M", "IQ2_XS", "IQ2_S", "IQ2_M", "IQ3_XS", "IQ3_S",
        "IQ3_M", "IQ4_XS", "IQ4_NL", "Q2_K_S", "Q3_K_S", "Q3_K_M", "Q3_K_L", "Q4_K_S", "Q4_K_M",
        "Q4_K_L", "Q5_K_S", "Q5_K_M", "Q5_K_L", "Q2_K", "Q4_0", "Q5_0", "Q6_K", "Q8_0", "F16",
        "BF16",
    ]
    .into_iter()
    .find(|quant| upper.contains(quant))
    .map(str::to_owned)
}

fn parameter_size(value: &str) -> Option<String> {
    value
        .split(['-', '_'])
        .find(|part| {
            let lower = part.to_ascii_lowercase();
            lower.ends_with('b') && lower[..lower.len() - 1].parse::<f32>().is_ok()
        })
        .map(str::to_owned)
}

fn download_url(repository: &str, revision: &str, artifact: &str) -> Result<String, ModelError> {
    let mut url = Url::parse("https://huggingface.co")
        .map_err(|error| ModelError::Provider(error.to_string()))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| ModelError::Provider("invalid provider download URL".to_owned()))?;
        for segment in repository.split('/') {
            segments.push(segment);
        }
        segments.push("resolve").push(revision).push(artifact);
    }
    url.query_pairs_mut().append_pair("download", "true");
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hardware(runtime: bool, memory: u64) -> HardwareCapabilities {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "cpu_architecture": "x86_64",
            "cpu_brand": null,
            "logical_cpu_count": 4,
            "cpu_features": [],
            "total_memory_bytes": memory,
            "available_memory_bytes": memory,
            "gpu": { "vendor": null, "name": null, "kind": "unknown", "usable_memory_bytes": null, "memory_note": "test" },
            "model_storage": { "root": "/tmp", "free_bytes": 10_000_000_000_u64, "free_bytes_note": "test" },
            "runtime": { "llama_cpp": { "available": runtime, "executable": null, "version": null, "reason": "runtime missing" } },
            "supported_formats": [],
            "supported_quantizations": []
        })).unwrap()
    }

    #[allow(clippy::needless_pass_by_value)]
    fn model(files: serde_json::Value, gated: serde_json::Value) -> HfModel {
        serde_json::from_value(serde_json::json!({
            "id": "owner/Llama-3.2-3B-GGUF",
            "sha": "0123456789012345678901234567890123456789",
            "private": false,
            "gated": gated,
            "tags": ["license:apache-2.0"],
            "siblings": files,
            "gguf": { "architecture": "llama", "context_length": 131_072, "chat_template": "template" }
        })).unwrap()
    }

    #[test]
    fn ollama_names_are_search_aliases() {
        assert_eq!(normalize_query("llama3.2:3b"), "Llama 3.2 3B GGUF");
        assert_eq!(normalize_query("owner/model"), "owner/model");
    }

    #[test]
    fn accepts_only_exact_single_file_gguf_selections() {
        assert!(is_single_gguf("model-Q4_K_M.gguf"));
        assert!(!is_single_gguf("model.safetensors"));
        assert!(!is_single_gguf("model-00001-of-00002.gguf"));
        assert!(!is_single_gguf("folder/model.gguf"));
    }

    #[test]
    fn extracts_multiple_quantizations() {
        assert_eq!(quantization("model-Q4_K_M.gguf").as_deref(), Some("Q4_K_M"));
        assert_eq!(quantization("model-q8_0.gguf").as_deref(), Some("Q8_0"));
    }

    #[test]
    fn recognizes_gated_metadata() {
        assert!(gated(&serde_json::json!(true)));
        assert!(gated(&serde_json::json!("manual")));
        assert!(!gated(&serde_json::json!(false)));
    }

    #[test]
    fn no_results_and_non_gguf_files_produce_no_candidates() {
        let empty = model(serde_json::json!([]), serde_json::json!(false));
        assert!(candidates_for(&empty, &hardware(true, u64::MAX)).is_empty());
        let incompatible = model(
            serde_json::json!([{ "rfilename": "model.safetensors", "size": 10, "lfs": { "sha256": "a", "size": 10 } }]),
            serde_json::json!(false),
        );
        assert!(candidates_for(&incompatible, &hardware(true, u64::MAX)).is_empty());
    }

    #[test]
    fn multiple_quantizations_are_distinct_exact_candidates() {
        let found = candidates_for(
            &model(
                serde_json::json!([
                    { "rfilename": "model-Q4_K_M.gguf", "size": 1000, "lfs": { "sha256": "a".repeat(64), "size": 1000 } },
                    { "rfilename": "model-Q8_0.gguf", "size": 2000, "lfs": { "sha256": "b".repeat(64), "size": 2000 } }
                ]),
                serde_json::json!(false),
            ),
            &hardware(true, u64::MAX),
        );
        assert_eq!(found.len(), 2);
        assert_ne!(found[0].candidate_id, found[1].candidate_id);
        assert_eq!(found[0].quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(found[1].quantization.as_deref(), Some("Q8_0"));
    }

    #[test]
    fn gated_or_incomplete_artifacts_are_visible_but_not_downloadable() {
        let files = serde_json::json!([
            { "rfilename": "model-Q4_K_M.gguf", "size": 1000, "lfs": { "sha256": "a".repeat(64), "size": 1000 } }
        ]);
        let gated = candidates_for(
            &model(files.clone(), serde_json::json!("manual")),
            &hardware(true, u64::MAX),
        );
        assert!(!gated[0].downloadable);
        assert_eq!(gated[0].access, "credential_required");

        let incomplete = candidates_for(
            &model(
                serde_json::json!([{ "rfilename": "model-Q4_K_M.gguf" }]),
                serde_json::json!(false),
            ),
            &hardware(true, u64::MAX),
        );
        assert!(!incomplete[0].downloadable);
        assert!(incomplete[0].bytes.is_none());
    }

    #[test]
    fn runtime_and_ram_incompatibility_are_reported_without_hiding_artifact() {
        let candidate = candidates_for(
            &model(
                serde_json::json!([{ "rfilename": "model-Q4_K_M.gguf", "size": 1000, "lfs": { "sha256": "a".repeat(64), "size": 1000 } }]),
                serde_json::json!(false),
            ),
            &hardware(false, 1),
        );
        assert_eq!(candidate.len(), 1);
        assert!(!candidate[0].runtime_compatible);
        assert!(candidate[0].downloadable);
    }
}
