use std::{path::Path, process::Stdio, time::Duration};

use serde::{Deserialize, Serialize};
use sysinfo::System;
use tokio::{process::Command, time::timeout};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareCapabilities {
    pub schema_version: u32,
    pub cpu_architecture: String,
    pub cpu_brand: Option<String>,
    pub logical_cpu_count: usize,
    pub cpu_features: Vec<String>,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: Option<u64>,
    pub gpu: GpuCapabilities,
    pub model_storage: StorageCapabilities,
    pub runtime: RuntimeCapabilities,
    pub supported_formats: Vec<String>,
    pub supported_quantizations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuCapabilities {
    pub vendor: Option<String>,
    pub name: Option<String>,
    pub kind: String,
    pub usable_memory_bytes: Option<u64>,
    pub memory_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageCapabilities {
    pub root: String,
    pub free_bytes: Option<u64>,
    pub free_bytes_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCapabilities {
    pub llama_cpp: RuntimeAvailability,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeAvailability {
    pub available: bool,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub reason: String,
}

pub async fn detect(model_root: &Path) -> HardwareCapabilities {
    let mut system = System::new_all();
    system.refresh_all();
    let cpu_brand = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().trim().to_owned())
        .filter(|brand| !brand.is_empty());
    let runtime = detect_llama_runtime().await;
    let gpu = detect_gpu().await;
    let free_bytes = fs2::available_space(model_root).ok();
    let executable_runtime = runtime.available;
    HardwareCapabilities {
        schema_version: 1,
        cpu_architecture: std::env::consts::ARCH.to_owned(),
        cpu_brand,
        logical_cpu_count: system.cpus().len(),
        cpu_features: cpu_features(),
        total_memory_bytes: system.total_memory(),
        available_memory_bytes: (system.available_memory() > 0)
            .then_some(system.available_memory()),
        gpu,
        model_storage: StorageCapabilities {
            root: model_root.display().to_string(),
            free_bytes,
            free_bytes_note: if free_bytes.is_some() {
                "Reported by the filesystem containing the configured model root.".to_owned()
            } else {
                "Free space could not be determined.".to_owned()
            },
        },
        runtime: RuntimeCapabilities { llama_cpp: runtime },
        supported_formats: if executable_runtime {
            vec!["gguf".to_owned()]
        } else {
            Vec::new()
        },
        supported_quantizations: if executable_runtime {
            [
                "Q2_K", "Q3_K_M", "Q4_0", "Q4_K_M", "Q5_0", "Q5_K_M", "Q6_K", "Q8_0",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        } else {
            Vec::new()
        },
    }
}

async fn detect_llama_runtime() -> RuntimeAvailability {
    let explicit = std::env::var_os("TOME_LLAMA_SERVER_PATH").map(std::path::PathBuf::from);
    let executable = explicit.unwrap_or_else(|| {
        if cfg!(windows) {
            "llama-server.exe".into()
        } else {
            "llama-server".into()
        }
    });
    let result = timeout(
        Duration::from_secs(3),
        Command::new(&executable)
            .arg("--version")
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .output(),
    )
    .await;
    match result {
        Ok(Ok(output)) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let version = (!stdout.is_empty())
                .then_some(stdout)
                .or_else(|| (!stderr.is_empty()).then_some(stderr));
            RuntimeAvailability {
                available: true,
                executable: Some(executable.display().to_string()),
                version,
                reason: "A llama.cpp server executable responded successfully.".to_owned(),
            }
        }
        Ok(Ok(output)) => RuntimeAvailability {
            available: false,
            executable: Some(executable.display().to_string()),
            version: None,
            reason: format!("The llama.cpp executable exited with {}.", output.status),
        },
        Ok(Err(_)) => RuntimeAvailability {
            available: false,
            executable: None,
            version: None,
            reason: "llama-server was not found. Install a reviewed llama.cpp build or set TOME_LLAMA_SERVER_PATH.".to_owned(),
        },
        Err(_) => RuntimeAvailability {
            available: false,
            executable: Some(executable.display().to_string()),
            version: None,
            reason: "The llama.cpp version probe timed out after three seconds.".to_owned(),
        },
    }
}

#[allow(clippy::unused_async)]
async fn detect_gpu() -> GpuCapabilities {
    #[cfg(windows)]
    {
        let script = "Get-CimInstance Win32_VideoController | Select-Object -First 1 Name,AdapterRAM | ConvertTo-Json -Compress";
        if let Ok(Ok(output)) = timeout(
            Duration::from_secs(4),
            Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", script])
                .kill_on_drop(true)
                .output(),
        )
        .await
            && output.status.success()
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        {
            let name = value
                .get("Name")
                .and_then(|value| value.as_str())
                .map(str::to_owned);
            let memory = value.get("AdapterRAM").and_then(serde_json::Value::as_u64);
            let vendor = name.as_deref().and_then(gpu_vendor).map(str::to_owned);
            return GpuCapabilities {
                vendor,
                name,
                kind: "windows_video_controller".to_owned(),
                usable_memory_bytes: memory,
                memory_note: if memory.is_some() {
                    "AdapterRAM is firmware-reported capacity, not a live allocation guarantee."
                        .to_owned()
                } else {
                    "Windows did not report adapter memory reliably.".to_owned()
                },
            };
        }
    }
    #[cfg(target_os = "macos")]
    {
        GpuCapabilities {
            vendor: Some("Apple".to_owned()),
            name: Some("Apple integrated GPU".to_owned()),
            kind: "unified_memory".to_owned(),
            usable_memory_bytes: None,
            memory_note: "Apple GPU memory is dynamically shared; Tome does not invent a dedicated VRAM value.".to_owned(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        GpuCapabilities {
            vendor: None,
            name: None,
            kind: "unknown".to_owned(),
            usable_memory_bytes: None,
            memory_note: "No bounded, reliable GPU memory probe is available on this platform."
                .to_owned(),
        }
    }
}

#[cfg(windows)]
fn gpu_vendor(name: &str) -> Option<&'static str> {
    let lowercase = name.to_ascii_lowercase();
    if lowercase.contains("nvidia") {
        Some("NVIDIA")
    } else if lowercase.contains("amd") || lowercase.contains("radeon") {
        Some("AMD")
    } else if lowercase.contains("intel") {
        Some("Intel")
    } else {
        None
    }
}

#[allow(clippy::vec_init_then_push)]
fn cpu_features() -> Vec<String> {
    let mut features = Vec::new();
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    for (name, enabled) in [
        ("sse4.2", std::arch::is_x86_feature_detected!("sse4.2")),
        ("avx", std::arch::is_x86_feature_detected!("avx")),
        ("avx2", std::arch::is_x86_feature_detected!("avx2")),
        ("fma", std::arch::is_x86_feature_detected!("fma")),
    ] {
        if enabled {
            features.push(name.to_owned());
        }
    }
    #[cfg(target_arch = "aarch64")]
    features.push("aarch64".to_owned());
    features
}
