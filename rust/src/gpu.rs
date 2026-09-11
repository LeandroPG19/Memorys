use anyhow::Result;
use ort::session::builder::SessionBuilder;
#[cfg(any(feature = "cuda", feature = "directml"))]
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workload {
    Embedder,
    Reranker,
    Nli,
}

impl Workload {
    fn device_var(self) -> &'static str {
        match self {
            Self::Embedder => "CUBA_EMBED_DEVICE",
            Self::Reranker => "CUBA_RERANK_DEVICE",
            Self::Nli => "CUBA_NLI_DEVICE",
        }
    }

    fn gpu_by_default(self) -> bool {
        matches!(self, Self::Reranker)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Embedder => "embedder",
            Self::Reranker => "reranker",
            Self::Nli => "nli",
        }
    }
}

pub fn wants_gpu(workload: Workload) -> bool {
    if !cfg!(any(feature = "cuda", feature = "directml")) {
        return false;
    }
    match std::env::var(workload.device_var()) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "gpu" | "cuda" | "directml" => true,
            "cpu" => false,
            other => {
                tracing::warn!(
                    var = workload.device_var(),
                    value = other,
                    default_gpu = workload.gpu_by_default(),
                    "dispositivo desconocido — uso el default del modelo"
                );
                workload.gpu_by_default()
            }
        },
        Err(_) => workload.gpu_by_default(),
    }
}

pub fn configure(builder: SessionBuilder, workload: Workload) -> Result<SessionBuilder> {
    if !wants_gpu(workload) {
        return configure_cpu(builder, workload);
    }

    let providers: Vec<ort::ep::ExecutionProviderDispatch> = [
        #[cfg(feature = "cuda")]
        cuda_provider(),
        #[cfg(feature = "directml")]
        ort::ep::DirectML::default().build(),
    ]
    .into_iter()
    .collect();

    if providers.is_empty() {
        return configure_cpu(builder, workload);
    }

    tracing::info!(model = workload.label(), "sesión ONNX en GPU");
    builder
        .with_execution_providers(providers)
        .map_err(|e| anyhow::anyhow!("registrando execution providers GPU: {e}"))
}

fn configure_cpu(builder: SessionBuilder, workload: Workload) -> Result<SessionBuilder> {
    let use_arena = !matches!(workload, Workload::Nli);
    tracing::info!(
        model = workload.label(),
        arena = use_arena,
        "sesión ONNX en CPU"
    );
    builder
        .with_execution_providers([ort::ep::CPU::default()
            .with_arena_allocator(use_arena)
            .build()])
        .map_err(|e| anyhow::anyhow!("registrando CPU execution provider: {e}"))
}

#[cfg(feature = "cuda")]
fn cuda_provider() -> ort::ep::ExecutionProviderDispatch {
    let limit_mb: usize = std::env::var("CUBA_GPU_MEM_LIMIT_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2048);

    ort::ep::CUDA::default()
        .with_arena_extend_strategy(ort::ep::ArenaExtendStrategy::SameAsRequested)
        .with_memory_limit(limit_mb * 1024 * 1024)
        .build()
}

pub struct GpuStatus {
    pub degraded: bool,
    pub detail: String,
    pub hint: Option<String>,
}

pub fn placement_summary() -> String {
    [Workload::Embedder, Workload::Reranker, Workload::Nli]
        .iter()
        .map(|&w| {
            let dev = if wants_gpu(w) { "gpu" } else { "cpu" };
            format!("{}={dev}", w.label())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn status() -> GpuStatus {
    #[cfg(any(feature = "cuda", feature = "directml"))]
    {
        let provider = if cfg!(feature = "cuda") {
            "cuda"
        } else {
            "directml"
        };
        let runtime_gpu = runtime_has_gpu_provider(provider);
        let gpu_device = provider != "cuda" || nvidia_present();

        if runtime_gpu && gpu_device {
            return GpuStatus {
                degraded: false,
                detail: format!(
                    "{provider} — runtime GPU y GPU detectados · colocación: {}",
                    placement_summary()
                ),
                hint: None,
            };
        }
        if !runtime_gpu {
            return GpuStatus {
                degraded: true,
                detail: format!(
                    "compilado con {provider}, pero el runtime instalado es el de CPU → corriendo en CPU"
                ),
                hint: Some("cuba-memorys models runtime --gpu".to_string()),
            };
        }
        GpuStatus {
            degraded: true,
            detail: format!(
                "compilado con {provider}, pero no detecté GPU NVIDIA → corriendo en CPU"
            ),
            hint: Some(
                "revisá el driver (nvidia-smi); sin GPU, esta build igual corre en CPU".to_string(),
            ),
        }
    }
    #[cfg(all(not(feature = "cuda"), not(feature = "directml")))]
    {
        if nvidia_driver_present() {
            return GpuStatus {
                degraded: true,
                detail: "hay una GPU NVIDIA en esta máquina pero el binario se compiló sin \
                         soporte — el embebedor corre en CPU y cada búsqueda cuesta ~9x más"
                    .to_string(),
                hint: Some("./scripts/build-gpu.sh".to_string()),
            };
        }
        GpuStatus {
            degraded: false,
            detail: "cpu (compilado sin soporte GPU)".to_string(),
            hint: None,
        }
    }
}

pub fn active_provider() -> String {
    status().detail
}

#[cfg(any(feature = "cuda", feature = "directml"))]
fn runtime_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
        return PathBuf::from(p).parent().map(|p| p.to_path_buf());
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let cache = PathBuf::from(home).join(".cache");
    let preferred = cache.join("memory-industry").join("onnxruntime");
    let legacy = cache.join("cuba-memorys").join("onnxruntime");
    Some(if preferred.exists() || !legacy.exists() {
        preferred
    } else {
        legacy
    })
}

#[cfg(any(feature = "cuda", feature = "directml"))]
fn runtime_has_gpu_provider(provider: &str) -> bool {
    let Some(dir) = runtime_dir() else {
        return false;
    };
    let candidates: [&str; 4] = match provider {
        "cuda" => [
            "libonnxruntime_providers_cuda.so",
            "onnxruntime_providers_cuda.dll",
            "libonnxruntime_providers_cuda.dylib",
            "onnxruntime_providers_cuda.so",
        ],
        _ => [
            "onnxruntime_providers_dml.dll",
            "DirectML.dll",
            "libonnxruntime_providers_dml.so",
            "onnxruntime_providers_dml.so",
        ],
    };
    candidates.iter().any(|name| dir.join(name).exists())
}

#[cfg_attr(any(feature = "cuda", feature = "directml"), allow(dead_code))]
fn nvidia_driver_present() -> bool {
    if std::path::Path::new("/proc/driver/nvidia/version").exists() {
        return true;
    }
    let exe = if cfg!(windows) {
        "nvidia-smi.exe"
    } else {
        "nvidia-smi"
    };
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|p| p.join(exe).exists()))
        .unwrap_or(false)
}

#[cfg(feature = "cuda")]
fn nvidia_present() -> bool {
    if std::path::Path::new("/proc/driver/nvidia/version").exists() {
        return true;
    }
    let exe = if cfg!(windows) {
        "nvidia-smi.exe"
    } else {
        "nvidia-smi"
    };
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|p| p.join(exe).exists()))
        .unwrap_or(false)
}
