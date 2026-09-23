use anyhow::Result;
use ort::session::builder::SessionBuilder;
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

    /// The `MEMORY_INDUSTRY_*` spelling, where there is one. Only the reranker
    /// has been promoted: it is the placement an operator actually sets, and
    /// the one this release documents under the new namespace. Promoting the
    /// other two would add two knobs to `.env.example` that nobody has ever
    /// had to touch.
    fn preferred_device_var(self) -> Option<&'static str> {
        match self {
            Self::Reranker => Some("MEMORY_INDUSTRY_RERANK_DEVICE"),
            Self::Embedder | Self::Nli => None,
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
    let configured = match workload.preferred_device_var() {
        Some(preferred) => crate::envs::alias(preferred, workload.device_var()),
        None => std::env::var(workload.device_var()),
    };
    match configured {
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
        provider != "cuda" || nvidia_driver_present(),
    )
}

#[cfg(not(any(feature = "cuda", feature = "directml")))]
fn gpu_availability() -> (bool, bool) {
    // Both halves measured, including the one this build cannot use. It could
    // not load either provider library, but it can see whether they are on
    // disk, and that is the difference between an operator who still has to
    // download the runtime and one who only has to build — two trips or one.
    // The probe costs two `Path::exists` calls and no dependency: `ort` is
    // linked unconditionally and neither function below touches it.
    (
        runtime_has_gpu_provider("cuda") || runtime_has_gpu_provider("directml"),
        nvidia_driver_present(),
    )
}

/// Takes a factory rather than a builder: when the GPU provider refuses to
/// start we need a second, clean builder for the CPU path, and a
/// `SessionBuilder` is consumed by the attempt.
///
/// It is also the one door the embedder, NLI and the reranker all pass
/// before `Session::builder()`, so the runtime is loaded here, from the path
/// `locate_onnxruntime` found. Left to `ort`, whichever model opened first
/// decided: the reranker, which never searched, got the bare
/// `onnxruntime.dll` — on Windows the Windows ML 1.17 in System32.
pub fn configure<F>(make_builder: F, workload: Workload) -> Result<SessionBuilder>
where
    F: Fn() -> Result<SessionBuilder>,
{
    crate::embeddings::onnx::load_located_onnxruntime()?;
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
    let limit_mb: usize =
        crate::envs::alias("MEMORY_INDUSTRY_GPU_MEM_LIMIT_MB", "CUBA_GPU_MEM_LIMIT_MB")
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

/// The GPU provider this binary was built against, if any.
///
/// The only feature flag read on the placement path. Everything downstream
/// takes it as a value, which is what lets the CI box — no card, and never
/// will have one — execute the branches a CUDA build takes.
fn compiled_provider() -> Option<&'static str> {
    if cfg!(feature = "cuda") {
        Some("cuda")
    } else if cfg!(feature = "directml") {
        Some("directml")
    } else {
        None
    }
}

/// What to tell the operator, given what the build is and what the machine has.
///
/// Split from `status()` so the *judgement* can be read anywhere: the failure
/// this whole path exists for was never a kernel landing on the wrong device,
/// it was a machine being told the wrong reason and the operator fixing the
/// wrong thing. `compiled` carries the provider name rather than a bare flag
/// because the name is in half of the eight messages, and taking it as a value
/// is what keeps `cfg!` out of here entirely.
///
/// The one thing it reads beyond its arguments is `placement_summary()`, on
/// the single row where nothing is missing.
pub fn status_from(compiled: Option<&str>, runtime_gpu: bool, device_present: bool) -> GpuStatus {
    let Some(provider) = compiled else {
        return status_without_a_gpu_build(runtime_gpu, device_present);
    };

    match (runtime_gpu, device_present) {
        (true, true) => GpuStatus {
            degraded: false,
            detail: format!(
                "{provider} — runtime GPU y GPU detectados · colocación: {}",
                placement_summary()
            ),
            hint: None,
        },
        (false, true) => GpuStatus {
            degraded: true,
            detail: format!(
                "compilado con {provider}, pero el runtime instalado es el de CPU → corriendo en CPU"
            ),
            hint: Some("memory-industry models runtime --gpu".to_string()),
        },
        (true, false) => GpuStatus {
            degraded: true,
            detail: format!(
                "compilado con {provider}, pero no detecté GPU NVIDIA → corriendo en CPU"
            ),
            hint: Some(
                "revisá el driver (nvidia-smi); sin GPU, esta build igual corre en CPU".to_string(),
            ),
        },
        // Missing both used to read exactly like missing the runtime alone:
        // the operator downloaded a GPU runtime and only then found out there
        // was no card to use it with. Two causes, one sentence, two trips.
        (false, false) => GpuStatus {
            degraded: true,
            detail: format!(
                "compilado con {provider}, pero no hay runtime GPU instalado ni GPU NVIDIA visible → corriendo en CPU"
            ),
            hint: Some(
                "faltan las dos cosas: el runtime GPU (`memory-industry models runtime --gpu`) \
                 y una tarjeta que nvidia-smi vea. Instalar solo el runtime no cambia nada en \
                 esta máquina"
                    .to_string(),
            ),
        },
    }
}

/// The four machines a binary built without any GPU feature can find itself on.
///
/// `runtime_gpu` is a measurement here, not the constant it used to be: this
/// build cannot load a provider library, but looking for one costs a
/// `Path::exists` and tells the operator which half of the work is left. While
/// the two rows collapsed, a machine with the runtime already downloaded read
/// exactly like one with nothing at all, and the only way to find out which
/// was to do both steps.
///
/// Kept out of `status_from` because it answers a different question — what a
/// CPU build can say — and inlining it put six arms and a let-else in one
/// function for no reader's benefit.
fn status_without_a_gpu_build(runtime_gpu: bool, device_present: bool) -> GpuStatus {
    match (runtime_gpu, device_present) {
        (true, true) => GpuStatus {
            degraded: true,
            detail: "hay una GPU NVIDIA y el runtime GPU ya instalado, pero el binario se \
                     compiló sin soporte — solo falta la build: el embebedor corre en CPU y \
                     cada búsqueda cuesta ~9x más"
                .to_string(),
            hint: Some("./scripts/build-gpu.sh".to_string()),
        },
        (false, true) => GpuStatus {
            degraded: true,
            detail: "hay una GPU NVIDIA en esta máquina pero el binario se compiló sin \
                     soporte — el embebedor corre en CPU y cada búsqueda cuesta ~9x más"
                .to_string(),
            hint: Some("./scripts/build-gpu.sh".to_string()),
        },
        // Not degraded and no hint, on purpose. Nothing is being wasted here —
        // there is no card — and the one exit this repo has, `build-gpu.sh`,
        // builds `--features cuda`, so pointing a machine nvidia-smi cannot see
        // at it is the wrong trip in the other direction. The sentence still
        // reports what is on disk, because that is what nobody could see
        // before.
        (true, false) => GpuStatus {
            degraded: false,
            detail: "cpu — el runtime GPU está instalado pero no hay GPU NVIDIA visible, y \
                     este binario tampoco se compiló con soporte"
                .to_string(),
            hint: None,
        },
        (false, false) => GpuStatus {
            degraded: false,
            detail: "cpu (compilado sin soporte GPU)".to_string(),
            hint: None,
        },
    }
}

pub fn status() -> GpuStatus {
    let (runtime_gpu, device_present) = gpu_availability();
    status_from(compiled_provider(), runtime_gpu, device_present)
}

pub fn active_provider() -> String {
    status().detail
}

fn runtime_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
        return PathBuf::from(p).parent().map(|p| p.to_path_buf());
    }
    // `.ok()?`: `runtime_has_gpu_provider` reads None as «nothing downloaded»
    // and drops to the CPU, which is right for a machine that never ran
    // `models runtime` — not a fault to report.
    let cache = crate::envs::home().ok()?.join(".cache");
    let preferred = cache.join("memory-industry").join("onnxruntime");
    // A directory that exists on operators' disks, not the binary's name: a
    // rename sweep that greps for the old name has to leave this one alone or
    // every runtime already downloaded is orphaned.
    let legacy = cache.join("cuba-memorys").join("onnxruntime");
    Some(if preferred.exists() || !legacy.exists() {
        preferred
    } else {
        legacy
    })
}

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

/// Is there a card to talk to: the driver's own file, or `nvidia-smi` on PATH.
///
/// One function, called from both arms of `gpu_availability`. It used to be two
/// byte-identical twins under opposite `cfg`s, which meant neither could ever
/// be told apart from the other by a test or by a mutant — the same argument
/// that collapsed `restrict` into one body in 0.27.
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

#[cfg(test)]
mod placement_tests {
    use super::*;
    use crate::envs::{ScopedEnv, scratch_root};

    /// Whether this binary can place anything on a card at all.
    ///
    /// `wants_gpu` short-circuits on this before reading a single variable, so
    /// the table below runs in two regimes: on a build without a GPU feature
    /// it proves that the short circuit is there — delete it and every `gpu`
    /// row turns red — and under `--features cuda` the same rows prove the
    /// parsing. A green run on the CI box does *not* mean the parsing was
    /// exercised, which is the whole reason this constant is named instead of
    /// written inline.
    const GPU_COMPILED: bool = cfg!(any(feature = "cuda", feature = "directml"));

    /// The same fact with the provider's name on it, which is what `status()`
    /// hands to the judging half. Spelled out rather than taken from
    /// `compiled_provider()`: a test that asks the function under test for the
    /// expectation it is checking agrees with every answer that function can
    /// give, including a wrong one.
    const COMPILED_PROVIDER: Option<&str> = if cfg!(feature = "cuda") {
        Some("cuda")
    } else if cfg!(feature = "directml") {
        Some("directml")
    } else {
        None
    };

    /// Every device variable the crate reads, cleared before each row so a row
    /// only ever says what it sets. `MEMORY_INDUSTRY_EMBED_DEVICE` is in the
    /// list although nothing reads it: the last row asserts exactly that, and
    /// it cannot assert it while the machine running the suite might have the
    /// variable set.
    const DEVICE_VARS: [&str; 5] = [
        "MEMORY_INDUSTRY_RERANK_DEVICE",
        "CUBA_RERANK_DEVICE",
        "MEMORY_INDUSTRY_EMBED_DEVICE",
        "CUBA_EMBED_DEVICE",
        "CUBA_NLI_DEVICE",
    ];

    #[tokio::test]
    async fn the_device_variable_decides_and_a_typo_falls_back_to_the_model_default() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        // (workload, what the environment says, wanted on a GPU build, why).
        // Two fixed slots rather than a slice so every row has one type and the
        // table needs no annotation to hold together.
        let table = [
            (
                Workload::Reranker,
                [None, None],
                true,
                "unset means the model's own default, and the reranker's is the card. Every                  installation in the field that never touched the variable expects that",
            ),
            (
                Workload::Embedder,
                [None, None],
                false,
                "the embedder's default is the CPU. One default shared by every model would                  put three sessions on one card and make the arena ceiling meaningless",
            ),
            (
                Workload::Nli,
                [None, None],
                false,
                "and NLI is the same: it runs once per candidate pair, not once per search",
            ),
            (
                Workload::Reranker,
                [Some(("CUBA_RERANK_DEVICE", "cpu")), None],
                false,
                "an explicit cpu has to beat the model default, or an operator cannot turn the                  card off after a bad night. It is also the row that catches a build that                  stopped reading the legacy name, or that read the reranker out of another                  model's variable",
            ),
            (
                Workload::Reranker,
                [Some(("CUBA_RERANK_DEVICE", "  CPU  ")), None],
                false,
                "an env file edited by hand carries the spaces and the capitals that were                  typed. On the reranker on purpose: its default is the card, so losing the                  trim or the lowercasing flips the answer instead of landing back on it",
            ),
            (
                Workload::Embedder,
                [Some(("CUBA_EMBED_DEVICE", "gpu")), None],
                true,
                "`gpu` on a model whose default is the CPU. The same row on the reranker                  would still pass with the word deleted from the match, because the fallback                  agrees with it",
            ),
            (
                Workload::Embedder,
                [Some(("CUBA_EMBED_DEVICE", "cuda")), None],
                true,
                "an operator who writes the provider name instead of `gpu` means the same thing",
            ),
            (
                Workload::Embedder,
                [Some(("CUBA_EMBED_DEVICE", "directml")), None],
                true,
                "and so does the Windows one, whichever provider this build carries",
            ),
            (
                Workload::Nli,
                [Some(("CUBA_NLI_DEVICE", "gpu")), None],
                true,
                "NLI answers to its own variable. Without this row the three placements could                  be read out of one shared name and nothing here would notice",
            ),
            (
                Workload::Reranker,
                [Some(("CUBA_RERANK_DEVICE", "gpuu")), None],
                true,
                "a value nobody recognises falls back to the model default — the card, here.                  Today the only trace of that decision is a warn! nothing reads, which is why                  nobody could say which way it fell",
            ),
            (
                Workload::Embedder,
                [Some(("CUBA_EMBED_DEVICE", "gpuu")), None],
                false,
                "the same typo on a model whose default is the CPU. With the row above it                  pins the fallback to the model rather than to a constant: a constant has to                  disagree with one of the two",
            ),
            (
                Workload::Reranker,
                [
                    Some(("MEMORY_INDUSTRY_RERANK_DEVICE", "gpu")),
                    Some(("CUBA_RERANK_DEVICE", "cpu")),
                ],
                true,
                "the documented name beats the legacy line an operator forgot to delete from                  the unit file",
            ),
            (
                Workload::Reranker,
                [
                    Some(("MEMORY_INDUSTRY_RERANK_DEVICE", "cpu")),
                    Some(("CUBA_RERANK_DEVICE", "gpu")),
                ],
                false,
                "and the other way round, or the precedence would only be pinned in the                  direction that happens to agree with the model default",
            ),
            (
                Workload::Embedder,
                [Some(("MEMORY_INDUSTRY_EMBED_DEVICE", "gpu")), None],
                false,
                "only the reranker was promoted to the new namespace, so this variable is                  read by nobody. Pinned because it looks like it should work: promoting the                  other two has to be an edit here, not a surprise in the field",
            ),
        ];

        assert!(
            table.iter().any(|(_, _, wanted, _)| *wanted)
                && table.iter().any(|(_, _, wanted, _)| !*wanted),
            "the table has to carry both answers. With every row expecting the same one, a \
             build where GPU_COMPILED is false would be comparing a constant against itself \
             and calling it a table"
        );

        for (workload, environment, wanted_on_a_gpu_build, why) in table {
            let _cleared: Vec<ScopedEnv> = DEVICE_VARS
                .iter()
                .copied()
                .map(ScopedEnv::cleared)
                .collect();
            let _set: Vec<ScopedEnv> = environment
                .iter()
                .flatten()
                .map(|(name, value)| ScopedEnv::set(name, value))
                .collect();

            assert_eq!(
                wants_gpu(workload),
                GPU_COMPILED && wanted_on_a_gpu_build,
                "{} with {environment:?}: {why}",
                workload.label()
            );
        }
    }

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

    /// `status_from` is judged row by row elsewhere; this is the seam that
    /// feeds it. A wrong argument there changes no message and no measurement,
    /// and is the one failure this whole path exists to stop: a CUDA build that
    /// reports itself as compiled without support sends the operator to build
    /// what is already built, and a CPU build that claims a provider sends them
    /// to a driver that would not have helped.
    ///
    /// Judged against every sentence this build could honestly produce rather
    /// than against one, so it asserts nothing about what this machine has: the
    /// CI box with no card, a developer box with one, and a box with the
    /// runtime half installed take different rows and all three are right.
    #[tokio::test]
    async fn status_reports_the_provider_this_binary_was_compiled_with() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let said = status().detail;
        let honest: Vec<String> = [(false, false), (false, true), (true, false), (true, true)]
            .into_iter()
            .map(|(runtime_gpu, device_present)| {
                status_from(COMPILED_PROVIDER, runtime_gpu, device_present).detail
            })
            .collect();

        assert!(
            honest.contains(&said),
            "status() said «{said}», which no build compiled with {COMPILED_PROVIDER:?} can \
             say about any machine. The four it could say are {honest:?}. What is wrong here \
             is not the measurement — it is the provider handed to status_from"
        );
    }

    /// It used to be compiled out with the rest of the provider probe when no
    /// GPU feature was on, so the mutation gate — which builds without one —
    /// could not observe this decision at all, and `gpu.rs.*runtime_dir` sat in
    /// the exclusions of `quality-gate.sh` saying exactly that. The probe is
    /// measured in every build now, so this runs in every build, and that
    /// exclusion was deleted the same day.
    #[tokio::test]
    async fn a_runtime_downloaded_under_the_old_name_survives_the_rename() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let home = scratch_root("runtime-dir");
        let cache = home.join(".cache");
        let preferred = cache.join("memory-industry").join("onnxruntime");
        let legacy = cache.join("cuba-memorys").join("onnxruntime");

        let _no_override = ScopedEnv::cleared("ORT_DYLIB_PATH");
        let _home = ScopedEnv::set("HOME", &home.display().to_string());
        let _windows_home = ScopedEnv::set("USERPROFILE", &home.display().to_string());

        assert_eq!(
            runtime_dir().as_ref(),
            Some(&preferred),
            "with nothing downloaded yet the answer has to be the documented directory, or \
             `models runtime` writes one path and the loader looks in another"
        );

        std::fs::create_dir_all(&legacy).expect("the test owns this directory");
        assert_eq!(
            runtime_dir().as_ref(),
            Some(&legacy),
            "every machine that downloaded a runtime before the rename has it under \
             cuba-memorys. Preferring the new path when only the old one is on disk orphans \
             that download and the daemon drops to the CPU without saying why"
        );

        std::fs::create_dir_all(&preferred).expect("the test owns this directory");
        assert_eq!(
            runtime_dir().as_ref(),
            Some(&preferred),
            "and once the documented one exists it wins, or a machine that re-downloaded \
             under the new name would go on loading the stale copy for ever"
        );

        let beside_the_library = home.join("elsewhere");
        let by_hand = beside_the_library.join("onnxruntime.dll");
        let _explicit = ScopedEnv::set("ORT_DYLIB_PATH", &by_hand.display().to_string());
        assert_eq!(
            runtime_dir().as_ref(),
            Some(&beside_the_library),
            "an operator who points at a runtime by hand has the provider libraries beside \
             that file, not in the cache this crate manages"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// The half its neighbour above cannot see.
    ///
    /// That test points HOME and USERPROFILE at the same directory, so it pins
    /// neither which of the two is read nor what happens when neither is — and
    /// until now nothing else in the repository pinned it either. Both are
    /// contract: `runtime_has_gpu_provider` reads `None` as «nothing
    /// downloaded» and drops to the CPU, which is the right answer for a
    /// machine that never ran `models runtime`.
    #[tokio::test]
    async fn the_runtime_dir_prefers_home_and_gives_up_when_neither_name_is_set() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let _no_override = ScopedEnv::cleared("ORT_DYLIB_PATH");

        let chosen = scratch_root("runtime-home");
        let inherited = scratch_root("runtime-userprofile");
        let under_chosen = chosen
            .join(".cache")
            .join("memory-industry")
            .join("onnxruntime");
        let under_inherited = inherited
            .join(".cache")
            .join("memory-industry")
            .join("onnxruntime");

        {
            let _h = ScopedEnv::set("HOME", &chosen.display().to_string());
            let _u = ScopedEnv::set("USERPROFILE", &inherited.display().to_string());
            assert_eq!(
                runtime_dir().as_ref(),
                Some(&under_chosen),
                "HOME is the one an operator exports on purpose; USERPROFILE is the one the \
                 system exports for them. `models runtime` writes under the first of the two \
                 that answers, so reading them the other way round downloads into one \
                 directory and loads from another"
            );
        }
        {
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::set("USERPROFILE", &inherited.display().to_string());
            assert_eq!(
                runtime_dir().as_ref(),
                Some(&under_inherited),
                "PowerShell and cmd.exe define only USERPROFILE, which is every Windows \
                 operator who did not start from Git Bash"
            );
        }
        {
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::cleared("USERPROFILE");
            assert_eq!(
                runtime_dir(),
                None,
                "neither name set is «there is no runtime to find», said quietly. An error \
                 here would report a machine that simply never downloaded a runtime as a \
                 broken one, and the CPU path it is supposed to take is the supported one"
            );
        }
    }

    /// The half of the availability probe that is a filesystem question.
    ///
    /// It decides whether an operator is sent to download a runtime they
    /// already have, and since this release a build with no GPU feature asks it
    /// too — which is the only reason the two `compiled = None` rows stopped
    /// reading the same. `cargo mutants -- --lib` builds exactly that build, so
    /// `replace runtime_has_gpu_provider -> bool with true` had nothing to kill
    /// it until this existed.
    ///
    /// `ORT_DYLIB_PATH` rather than `HOME`: it is the branch `run-all-tests.sh`
    /// takes, and it pins the directory without depending on which of the two
    /// cache names happens to exist on the machine running the suite.
    #[tokio::test]
    async fn a_provider_library_beside_the_runtime_is_what_makes_it_a_gpu_runtime() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let dir = std::env::temp_dir().join(format!(
            "memory-industry-provider-lib-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("the test owns this directory");
        let _explicit = ScopedEnv::set(
            "ORT_DYLIB_PATH",
            &dir.join("onnxruntime.dll").display().to_string(),
        );

        assert!(
            !runtime_has_gpu_provider("cuda"),
            "an ONNX Runtime unpacked without --gpu is the library and nothing beside it. \
             Answering true there is how a machine is told its runtime is the GPU one and \
             then runs every search on the CPU"
        );
        assert!(
            !runtime_has_gpu_provider("directml"),
            "and the same for the other provider, or an empty directory would answer yes to \
             one of the two questions and the emptiness would never be the reason"
        );

        // One of the four names, not four: the loader needs one, and a check
        // that wanted all of them would report absent on every real install.
        std::fs::write(dir.join("onnxruntime_providers_cuda.dll"), b"")
            .expect("the test owns this file");
        assert!(
            runtime_has_gpu_provider("cuda"),
            "one provider library is what the loader opens; the four names are that same \
             library spelled for four platforms"
        );
        assert!(
            !runtime_has_gpu_provider("directml"),
            "the two lists have to stay apart. Merged, a CUDA-only runtime would report \
             DirectML present, and a Windows box would be told it has a provider it cannot \
             load"
        );

        std::fs::write(dir.join("DirectML.dll"), b"").expect("the test owns this file");
        assert!(
            runtime_has_gpu_provider("directml"),
            "DirectML ships its own library instead of an onnxruntime_providers_* one, which \
             is the whole reason that name is in the list"
        );

        let _ = std::fs::remove_dir_all(&dir);
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
