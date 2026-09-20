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

/// Why a workload is running on the CPU.
///
/// The distinction that matters is *precondition* versus *failure*. A machine
/// with no card, or with a runtime that was installed without the execution
/// provider libraries, is not broken — it just cannot use a GPU, and it must
/// keep working. Only a machine that has both and still cannot open the
/// provider has something wrong with it, and that one should be loud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuReason {
    /// The build has no GPU feature, or the device variable says `cpu`.
    NotAskedFor,
    /// No `onnxruntime_providers_*` beside the runtime library.
    NoRuntimeProvider,
    /// No driver and no `nvidia-smi`: there is no card to talk to.
    NoDevice,
}

/// Pure so it can be checked on any machine, including the CI box that has no
/// card and never will.
pub fn cpu_reason(wants_gpu: bool, runtime_gpu: bool, device_present: bool) -> Option<CpuReason> {
    if !wants_gpu {
        return Some(CpuReason::NotAskedFor);
    }
    if !runtime_gpu {
        return Some(CpuReason::NoRuntimeProvider);
    }
    if !device_present {
        return Some(CpuReason::NoDevice);
    }
    None
}

/// Whether the provider libraries and a device are actually there.
///
/// Split out of `configure` so the cfg walls live in one place: with them
/// inline, the function that decides placement had a different shape per
/// feature set and nothing about the decision was readable in either.
#[cfg(any(feature = "cuda", feature = "directml"))]
fn gpu_availability() -> (bool, bool) {
    let provider = if cfg!(feature = "cuda") {
        "cuda"
    } else {
        "directml"
    };
    (
        runtime_has_gpu_provider(provider),
        provider != "cuda" || nvidia_present(),
    )
}

#[cfg(not(any(feature = "cuda", feature = "directml")))]
fn gpu_availability() -> (bool, bool) {
    (false, false)
}

/// Takes a factory rather than a builder: when the GPU provider refuses to
/// start we need a second, clean builder for the CPU path, and a
/// `SessionBuilder` is consumed by the attempt.
pub fn configure<F>(make_builder: F, workload: Workload) -> Result<SessionBuilder>
where
    F: Fn() -> Result<SessionBuilder>,
{
    let builder = make_builder()?;
    let (runtime_gpu, device_present) = gpu_availability();

    if let Some(reason) = cpu_reason(wants_gpu(workload), runtime_gpu, device_present) {
        return fall_back_to_cpu(builder, workload, reason);
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

    match builder.with_execution_providers(providers) {
        Ok(configured) => {
            // After the registration, not before. This line used to be emitted
            // first, so a log could claim the session was on the GPU while ONNX
            // Runtime had quietly fallen back to the CPU underneath it.
            tracing::info!(model = workload.label(), "sesión ONNX en GPU");
            Ok(configured)
        }
        // The card and the provider libraries are both there and the provider
        // still would not start: the CUDA runtime itself is unusable here,
        // usually cudart/cublas/cuDNN missing from the loader path.
        //
        // That is a broken installation, not a broken model, and it must not
        // take the daemon down with it - this machine worked on CPU before and
        // has to keep working. But it must not be quiet either: without
        // `error_on_failure` ORT swallows this and runs on the CPU while every
        // log line and every check still says GPU. Loud, and on the CPU.
        Err(e) => {
            tracing::error!(
                model = workload.label(),
                error = %e,
                "el provider GPU no inicializó pese a haber tarjeta y librerías: esta sesión corre en CPU. Revisá que las DLL/so de CUDA estén en PATH/LD_LIBRARY_PATH junto al runtime de ONNX"
            );
            configure_cpu(make_builder()?, workload)
        }
    }
}

/// Wanting a GPU and not having one is worth saying once. It is not an error:
/// these machines have to keep working, which is the whole reason the
/// precondition is checked here instead of by letting the provider
/// registration fail.
/// Whether landing on the CPU is worth a line in the log.
///
/// Asking for a device and not getting it is; not asking is the common case
/// and saying so on every session would be noise nobody reads.
fn worth_warning(reason: CpuReason) -> bool {
    !matches!(reason, CpuReason::NotAskedFor)
}

fn fall_back_to_cpu(
    builder: SessionBuilder,
    workload: Workload,
    reason: CpuReason,
) -> Result<SessionBuilder> {
    if worth_warning(reason) {
        tracing::warn!(
            model = workload.label(),
            reason = ?reason,
            "se pidió GPU para este modelo y no está disponible — sigue en CPU"
        );
    }
    configure_cpu(builder, workload)
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
        // ORT defaults this to false and falls back to the CPU inside the
        // session, which is how a daemon ends up reporting a GPU it is not
        // using. Reaching here means the card and the provider libraries are
        // both present, so a registration that still fails is a broken
        // configuration somebody has to see.
        .error_on_failure()
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

#[cfg(test)]
mod placement_tests {
    use super::*;

    /// Every combination, so the next person to touch this cannot quietly turn
    /// a machine with no card into a hard error. Pure: it runs on the CI box.
    #[test]
    fn a_machine_with_no_gpu_says_cpu_and_a_broken_one_does_not_get_to_hide() {
        // (asked for a GPU, provider libs present, device present) -> reason
        let table = [
            (false, false, false, Some(CpuReason::NotAskedFor)),
            (false, true, true, Some(CpuReason::NotAskedFor)),
            (true, false, false, Some(CpuReason::NoRuntimeProvider)),
            (true, false, true, Some(CpuReason::NoRuntimeProvider)),
            (true, true, false, Some(CpuReason::NoDevice)),
            (true, true, true, None),
        ];
        for (wants, runtime, device, expected) in table {
            assert_eq!(
                cpu_reason(wants, runtime, device),
                expected,
                "wants_gpu={wants} runtime_provider={runtime} device={device}"
            );
        }
    }

    #[test]
    fn asking_for_a_gpu_on_a_machine_without_one_is_not_a_failure() {
        assert_eq!(
            cpu_reason(true, false, false),
            Some(CpuReason::NoRuntimeProvider),
            "a runtime installed without the execution-provider libraries is the common              shape of `models runtime` run without --gpu. It has to keep working on the CPU:              once the provider registration starts erroring, a None here would turn every one              of those machines into a daemon that refuses to load its reranker at all"
        );
        assert_eq!(
            cpu_reason(true, true, false),
            Some(CpuReason::NoDevice),
            "a CUDA build shipped to a machine with no card must degrade, not die"
        );
    }

    #[test]
    fn only_an_unmet_request_for_a_device_is_worth_a_log_line() {
        assert!(
            !worth_warning(CpuReason::NotAskedFor),
            "not asking for a device is the common case; a warning on every session is noise nobody reads, and noise is how a real warning gets missed"
        );
        for unmet in [CpuReason::NoRuntimeProvider, CpuReason::NoDevice] {
            assert!(
                worth_warning(unmet),
                "{unmet:?} means somebody configured a GPU and is not getting one. Silence there is how a deployment believes it is reranking on a card for months."
            );
        }
    }

    #[test]
    fn a_provider_that_will_not_start_drops_to_cpu_loudly_and_not_quietly() {
        // ort defaults error_on_failure to false, which means a CUDA provider
        // that cannot initialise is swallowed and the session runs on the CPU
        // while every log line still says GPU. Turning it on made that
        // visible - and also killed machines whose CUDA runtime simply is not
        // installed, which used to work on CPU. Both halves matter: the
        // fallback has to happen, and it has to be impossible to miss.
        let source = include_str!("gpu.rs");
        let body = source
            .split_once("pub fn configure<F>(")
            .expect("configure is in this file")
            .1;
        let body = body
            .split_once(
                "
}",
            )
            .expect("the function ends")
            .0;

        assert!(
            body.contains("error_on_failure") || source.contains("error_on_failure()"),
            "without error_on_failure the provider failure never reaches this code at all"
        );
        let failure = body.find("Err(e) => {").expect("the provider-failed arm");
        assert!(
            body[failure..].contains("tracing::error!"),
            "a GPU that silently became a CPU is the defect this whole path exists to stop: the fallback has to be logged at error"
        );
        assert!(
            body[failure..].contains("configure_cpu(make_builder()?"),
            "and it has to actually keep working. A machine without the CUDA runtime ran on CPU before and must keep doing so; failing the session there turns a slow daemon into a dead one."
        );
    }
}
