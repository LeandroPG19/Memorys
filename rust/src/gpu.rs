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

/// The one door a session opens through: configures the builder and hands it to
/// `commit`, which is the `commit_from_file` that makes the session exist.
/// The registry is written here, once `commit` has an answer, because
/// `configure` cannot know whether the commit will work: a refusal recorded
/// for a session that never opened would turn `gpu.degraded` true over nothing.
pub fn open<F, C, T>(make_builder: F, workload: Workload, commit: C) -> Result<T>
where
    F: Fn() -> Result<SessionBuilder>,
    C: FnOnce(SessionBuilder) -> Result<T>,
{
    let (builder, placement) = configure(make_builder, workload)?;
    let refusal = placement.clone();
    record_if_built(workload, placement, commit(builder))
        .map_err(|error| after_the_refusal(&refusal, error))
}

/// Between the provider's refusal and the commit's error in the one sentence
/// `/health` cuts on to name each half's paths by its own variable. It is ours
/// and not the `": "` that `anyhow` puts between the links of a chain, so the
/// cut does not depend on how a chain is joined.
pub(crate) const AND_THEN_ON_THE_CPU: &str = "; then the session did not open on the CPU either: ";

/// Takes a factory rather than a builder: when the GPU provider refuses to
/// start we need a second, clean builder for the CPU path, and a
/// `SessionBuilder` is consumed by the attempt.
///
/// It is also the one door the embedder, NLI and the reranker all pass
/// before `Session::builder()`, so the runtime is loaded here, from the path
/// `locate_onnxruntime` found. Left to `ort`, whichever model opened first
/// decided: the reranker, which never searched, got the bare
/// `onnxruntime.dll` — on Windows the Windows ML 1.17 in System32.
///
/// Returns where the session is going to open next to the builder and writes
/// nothing: `open` records it once the commit has worked.
fn configure<F>(make_builder: F, workload: Workload) -> Result<(SessionBuilder, Placement)>
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
        directml_provider(),
    ]
    .into_iter()
    .collect();

    if providers.is_empty() {
        return configure_cpu(builder, workload).map(|b| (b, Placement::Cpu));
    }

    match builder.with_execution_providers(providers) {
        Ok(configured) => {
            // After the registration, not before. This line used to be emitted
            // first, so a log could claim the session was on the GPU while ONNX
            // Runtime had quietly fallen back to the CPU underneath it.
            tracing::info!(model = workload.label(), "sesión ONNX en GPU");
            Ok((configured, Placement::Gpu))
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
                .map(|b| (b, Placement::CpuAfterProviderFailed(e.to_string())))
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
) -> Result<(SessionBuilder, Placement)> {
    if worth_warning(reason) {
        tracing::warn!(
            model = workload.label(),
            reason = ?reason,
            "se pidió GPU para este modelo y no está disponible — sigue en CPU"
        );
    }
    configure_cpu(builder, workload).map(|b| (b, Placement::Cpu))
}

/// Annotates the placement only once the session was committed: a builder that
/// could not become a session did not open, and must not leave a placement
/// behind for `/health` to report. A failed commit changes nothing, so the slot
/// stays as it was.
fn record_if_built<T>(workload: Workload, placement: Placement, built: Result<T>) -> Result<T> {
    built.inspect(|_| record(workload, placement))
}

/// The placement is the only place the provider's refusal lives, and a failed
/// commit throws it away with the rest of the placement: `/health` and
/// `doctor` would show the commit's error alone and the refusal would surface a
/// restart later. It goes in the error, flattened, and not in the registry,
/// which says where a session opened and this one did not.
fn after_the_refusal(placement: &Placement, error: anyhow::Error) -> anyhow::Error {
    match placement {
        Placement::CpuAfterProviderFailed(cause) => {
            anyhow::anyhow!("the GPU provider did not start: {cause}{AND_THEN_ON_THE_CPU}{error:#}")
        }
        Placement::Gpu | Placement::Cpu => error,
    }
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

/// The arena ceiling in MiB that the operator or the resource plan published, or
/// `None` when none did. Without one ONNX Runtime keeps its own limit, the card.
/// A number written here would be one nobody measured on this machine: 2048 was
/// that number, and it could not hold the 4,23 GiB reranker.
pub fn arena_limit_mb() -> Option<usize> {
    crate::envs::alias("MEMORY_INDUSTRY_GPU_MEM_LIMIT_MB", "CUBA_GPU_MEM_LIMIT_MB")
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
}

#[cfg(feature = "cuda")]
fn cuda_provider() -> ort::ep::ExecutionProviderDispatch {
    let cuda = ort::ep::CUDA::default()
        .with_arena_extend_strategy(ort::ep::ArenaExtendStrategy::SameAsRequested);
    let cuda = match arena_limit_mb() {
        Some(mb) => cuda.with_memory_limit(mb * 1024 * 1024),
        None => cuda,
    };

    cuda.build()
        // ORT defaults this to false and falls back to the CPU inside the
        // session, which is how a daemon ends up reporting a GPU it is not
        // using. Reaching here means the card and the provider libraries are
        // both present, so a registration that still fails is a broken
        // configuration somebody has to see.
        .error_on_failure()
}

/// DirectML asks to fail loudly only when it is the provider that decides.
/// With `cuda` in the build it registers behind a CUDA that already opened, so
/// a machine without `DirectML.dll` must keep its GPU session through CUDA
/// instead of turning into a refusal; without `cuda` nothing stands behind it
/// and a registration ORT swallows is a CPU session that reports as GPU.
#[cfg(feature = "directml")]
fn directml_provider() -> ort::ep::ExecutionProviderDispatch {
    let directml = ort::ep::DirectML::default().build();
    if cfg!(feature = "cuda") {
        directml
    } else {
        directml.error_on_failure()
    }
}

pub struct GpuStatus {
    pub degraded: bool,
    pub detail: String,
    pub hint: Option<String>,
}

/// Where a session actually opened, as `configure` saw it, and not where
/// `wants_gpu` said it should.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    Gpu,
    Cpu,
    /// The card and the provider libraries were there and the provider still
    /// would not start. Carries what ONNX Runtime said, whole.
    CpuAfterProviderFailed(String),
}

/// One slot per workload, in the order `slot` gives. The last write wins, so a
/// session that opens again after the DLLs were installed replaces the refusal
/// instead of sitting behind it for the life of the process. A poisoned lock is
/// read through: the registry is a set of independent slots and a panic in
/// another thread leaves each one whole.
static PLACEMENTS: std::sync::Mutex<[Option<Placement>; 3]> =
    std::sync::Mutex::new([None, None, None]);

fn slot(workload: Workload) -> usize {
    match workload {
        Workload::Embedder => 0,
        Workload::Reranker => 1,
        Workload::Nli => 2,
    }
}

fn record(workload: Workload, placement: Placement) {
    PLACEMENTS.lock().unwrap_or_else(|p| p.into_inner())[slot(workload)] = Some(placement);
}

/// Where this workload's session opened, or `None` while nobody opened one.
/// `wants_gpu` is the plan; this is what `configure` actually did.
pub fn placement(workload: Workload) -> Option<Placement> {
    PLACEMENTS.lock().unwrap_or_else(|p| p.into_inner())[slot(workload)].clone()
}

/// The workloads whose provider refused to start, in the fixed order embedder,
/// reranker, nli so two runs of the same machine print the same sentence.
fn refused_sessions() -> Vec<(Workload, String)> {
    [Workload::Embedder, Workload::Reranker, Workload::Nli]
        .into_iter()
        .filter_map(|workload| match placement(workload) {
            Some(Placement::CpuAfterProviderFailed(cause)) => Some((workload, cause)),
            _ => None,
        })
        .collect()
}

/// Folds what the sessions did into what `status_from` judged from the
/// machine. With nothing refused it is the identity. With a refusal the one
/// healthy row of `status_from` stops being healthy: the card and the provider
/// libraries were there and the session still opened on the CPU. The cause is
/// carried whole because `doctor` runs on the machine of whoever has to read it.
pub fn status_after_sessions(base: GpuStatus, refused: &[(Workload, String)]) -> GpuStatus {
    if refused.is_empty() {
        return base;
    }
    let sessions = refused
        .iter()
        .map(|(workload, cause)| format!("{}: {cause}", workload.label()))
        .collect::<Vec<_>>()
        .join(" · ");
    GpuStatus {
        degraded: true,
        detail: format!(
            "el provider GPU no inicializó → estas sesiones corren en CPU · {sessions}"
        ),
        hint: Some(
            "faltan DLL de CUDA 12 / cuDNN 9 junto al runtime de ONNX o en PATH (Windows) / \
             LD_LIBRARY_PATH (Linux): cudart64_12, cublas64_12, cublasLt64_12, cudnn64_9. La \
             colocación se decide al abrir la sesión: reiniciá el daemon después"
                .to_string(),
        ),
    }
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
    status_after_sessions(
        status_from(compiled_provider(), runtime_gpu, device_present),
        &refused_sessions(),
    )
}

/// The directory the provider libraries are looked for in: beside the runtime
/// `configure` loads, which is whatever `locate_onnxruntime` finds.
///
/// `ORT_DYLIB_PATH` is taken as the operator's word even when the file is not
/// there yet, which the search does not do. With nothing found anywhere, the
/// answer is where `models runtime` would put it; `runtime_has_gpu_provider`
/// then finds nothing there, and a `None` — no home at all — reads the same.
fn runtime_dir() -> Option<PathBuf> {
    if let Some(p) = crate::embeddings::onnx::explicit_runtime_path() {
        return p.parent().map(|p| p.to_path_buf());
    }
    match crate::embeddings::onnx::locate_onnxruntime() {
        Some(runtime) => runtime.parent().map(|p| p.to_path_buf()),
        None => crate::embeddings::onnx::runtime_cache_dir(),
    }
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

    type Row = (
        Option<&'static str>,
        Option<&'static str>,
        Option<usize>,
        &'static str,
    );

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

    /// An empty `ORT_DYLIB_PATH` does not decide where the providers are
    /// looked for.
    ///
    /// The env file `memory-industry setup service` writes carries
    /// `ORT_DYLIB_PATH=` as a placeholder and systemd exports it empty. The
    /// parent of an empty path is nothing, so `runtime_dir` answered `None`,
    /// `runtime_has_gpu_provider` read that as «no runtime downloaded» and the
    /// daemon reported no GPU provider on a machine that had one beside its
    /// runtime. Empty or blank means unset, as `locate_onnxruntime` and
    /// `ONNX_MODEL_PATH` already read it. A non-empty value is still the
    /// operator's word: its parent directory, whether or not the file exists.
    #[tokio::test]
    async fn an_empty_ort_dylib_path_does_not_decide_where_the_gpu_provider_is_looked_for() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let home = scratch_root("runtime-dir-empty-explicit");
        let _home = ScopedEnv::set("HOME", &home.display().to_string());
        let _windows_home = ScopedEnv::set("USERPROFILE", &home.display().to_string());
        let _no_ld = ScopedEnv::cleared("LD_LIBRARY_PATH");

        let unset = {
            let _no_override = ScopedEnv::cleared("ORT_DYLIB_PATH");
            runtime_dir()
        };
        assert!(
            unset.is_some(),
            "the fixture is broken: with a home and the variable unset `runtime_dir` has to              name some directory, or the comparisons below would pass on two Nones"
        );

        {
            let _empty = ScopedEnv::set("ORT_DYLIB_PATH", "");
            assert_eq!(
                runtime_dir(),
                unset,
                "`ORT_DYLIB_PATH=` comes out of the generated env file empty, and the parent                  of an empty path is nothing: the provider probe saw «no runtime» and the                  daemon said it had no GPU provider. Empty has to be read as unset"
            );
        }
        {
            let _blank = ScopedEnv::set("ORT_DYLIB_PATH", "   ");
            assert_eq!(
                runtime_dir(),
                unset,
                "a value of only spaces names no file and reads as unset too, the way an                  empty `ONNX_MODEL_PATH` does"
            );
        }
        {
            let beside_the_library = home.join("elsewhere");
            let by_hand = beside_the_library.join("onnxruntime.dll");
            let _explicit = ScopedEnv::set("ORT_DYLIB_PATH", &by_hand.display().to_string());
            assert_eq!(
                runtime_dir().as_ref(),
                Some(&beside_the_library),
                "control: a non-empty path is still the operator's word and its parent is                  where the providers are looked for. If this fell back to the search, the                  variable would be ignored altogether"
            );
        }

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

    /// The providers are wherever the runtime that gets loaded is, and that is
    /// not always the cache this crate manages.
    ///
    /// `configure` loads what `locate_onnxruntime` finds: the home cache, then
    /// LD_LIBRARY_PATH, then the system directories. The provider libraries
    /// were looked for only in the first of the three, so a machine whose
    /// runtime — providers beside it — lives in LD_LIBRARY_PATH loaded that
    /// runtime and was then told it had no GPU runtime at all, because the
    /// cache it was checked against holds nothing.
    ///
    /// The segment is relative to the process directory for the reason its
    /// neighbour in `embeddings/onnx.rs` gives: the split is on `':'` on every
    /// platform, which cuts a Windows absolute path at its drive letter.
    #[tokio::test]
    async fn the_providers_are_looked_for_beside_the_runtime_the_search_found() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let lib = if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else {
            "libonnxruntime.so"
        };
        let unique = scratch_root("gpu-ld");
        let beside_the_runtime = PathBuf::from("target").join(
            unique
                .file_name()
                .expect("scratch_root always ends in a uuid-suffixed name"),
        );
        std::fs::create_dir_all(&beside_the_runtime).expect("the process directory is writable");
        std::fs::write(beside_the_runtime.join(lib), b"not a real library")
            .expect("the process directory is writable");
        std::fs::write(
            beside_the_runtime.join("onnxruntime_providers_cuda.dll"),
            b"",
        )
        .expect("the process directory is writable");

        // A home with nothing under it, so the cache drops out of the chain
        // and the runtime the search settles on is the one in LD_LIBRARY_PATH.
        let empty_home = scratch_root("gpu-ld-home");
        let _explicit = ScopedEnv::cleared("ORT_DYLIB_PATH");
        let _h = ScopedEnv::set("HOME", &empty_home.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &empty_home.display().to_string());
        let _ld = ScopedEnv::set("LD_LIBRARY_PATH", &beside_the_runtime.display().to_string());

        // Positive control: without it, a search that never reached the
        // fixture would read below as the provider lookup being wrong.
        assert_eq!(
            crate::embeddings::onnx::locate_onnxruntime(),
            Some(beside_the_runtime.join(lib)),
            "the fixture is broken: the search does not settle on the runtime in \
             LD_LIBRARY_PATH, so what follows would measure something else"
        );
        assert_eq!(
            runtime_dir().as_ref(),
            Some(&beside_the_runtime),
            "the providers were looked for somewhere other than beside the runtime the \
             loader opens. With the runtime in LD_LIBRARY_PATH and nothing in the cache, \
             that is the empty cache, and the machine is told to download a GPU runtime it \
             already has"
        );
        assert!(
            runtime_has_gpu_provider("cuda"),
            "the provider library sits beside the runtime that is loaded; answering false \
             here sends a machine with a working GPU runtime to the CPU"
        );

        let _ = std::fs::remove_dir_all(&beside_the_runtime);
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
            .split_once("fn configure<F>(")
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
        // Structural on purpose, like the two checks above: this arm only runs
        // on a machine with a card whose provider refuses to start, and the
        // gate has none. What it does with the refusal is judged by
        // `status_after_sessions` and by the registry test below; that the arm
        // writes it, and writes *this* variant and not `Gpu` or `Cpu`, is
        // pinned here.
        assert!(
            body[failure..].contains("Placement::CpuAfterProviderFailed"),
            "the arm that drops to the CPU after the provider refused has to say so in the \
             registry. Without it `/health` and `doctor` go on reporting the device that was \
             asked for on a session that opened on the other one, which is the defect W1 exists \
             to stop"
        );
    }

    /// Without a plan nothing measured this card, so this crate has no number of
    /// its own to put on the arena. The constant that used to stand in for one
    /// (2048 MiB) was calibrated for `NextPowerOfTwo` growth, which
    /// `SameAsRequested` replaced; kept, it made the real reranker (4.23 GiB of
    /// weights) fail inside `commit_from_file` on every machine that opens it
    /// without `main`, which is every test, example and library user. Through
    /// the daemon `resources::apply` publishes the measured ceiling under the
    /// legacy name, and the `legacy` rows below are what keep that path alive.
    #[tokio::test]
    async fn without_a_published_plan_the_arena_has_no_ceiling_of_ours() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        const PREFERRED: &str = "MEMORY_INDUSTRY_GPU_MEM_LIMIT_MB";
        const LEGACY: &str = "CUBA_GPU_MEM_LIMIT_MB";

        // (preferred, legacy, wanted, why).
        let table: [Row; 7] = [
            (
                None,
                None,
                None,
                "no variable means no plan ran, and then nobody measured this card. Some(2048) is a \
                 number from another machine, and it is the one that makes the reranker fail \
                 inside the BFCArena",
            ),
            (
                Some("6000"),
                None,
                Some(6000),
                "the documented name has to be honoured: this is the control that the reader \
                 reads the variables at all",
            ),
            (
                None,
                Some("6000"),
                Some(6000),
                "`resources::apply` publishes the planned ceiling under the legacy name. A reader \
                 that only looked at the preferred one would drop the daemon's ceiling",
            ),
            (
                Some("7000"),
                Some("512"),
                Some(7000),
                "the preferred name wins when both are set, the same precedence `apply` uses",
            ),
            (
                Some(" 9000 "),
                None,
                Some(9000),
                "`resources::apply` trims before it parses, so the same text has to be a ceiling \
                 for the session too. Two readers of one variable cannot disagree",
            ),
            (
                Some("lots"),
                None,
                None,
                "an unreadable value is not a ceiling. It used to fall back to 2048 without a \
                 word",
            ),
            (
                None,
                Some("lots"),
                None,
                "the same holds through the legacy name",
            ),
        ];

        assert!(
            table.iter().any(|(_, _, wanted, _)| wanted.is_none())
                && table.iter().any(|(_, _, wanted, _)| wanted.is_some()),
            "the table has to carry both answers. With every row expecting the same one the \
             reader could be a constant and the table would still be green"
        );

        for (preferred, legacy, wanted, why) in table {
            let _cleared = [ScopedEnv::cleared(PREFERRED), ScopedEnv::cleared(LEGACY)];
            let _set: Vec<ScopedEnv> = [(PREFERRED, preferred), (LEGACY, legacy)]
                .into_iter()
                .filter_map(|(name, value)| value.map(|v| ScopedEnv::set(name, v)))
                .collect();

            assert_eq!(
                arena_limit_mb(),
                wanted,
                "{PREFERRED}={preferred:?} {LEGACY}={legacy:?}: {why}"
            );
        }
    }

    /// Structural, like the one above for `error_on_failure`: `cuda_provider`
    /// only compiles with `cuda` and only runs on a machine with a card, so
    /// neither the mutation build nor the WSL2 judge ever executes it. What it
    /// can be held to is where it gets its ceiling: from the one reader that
    /// the table above judges, and never from a number of its own.
    #[test]
    fn the_cuda_provider_takes_its_ceiling_from_the_one_reader() {
        let source = include_str!("gpu.rs");
        let body = source
            .split_once("fn cuda_provider()")
            .expect("cuda_provider is in this file")
            .1;
        let body = body.split_once("\n}").expect("the function ends").0;

        assert!(
            body.contains("arena_limit_mb()"),
            "cuda_provider has to ask `arena_limit_mb()` for its ceiling. Reading the variable \
             by itself, or not reading it, leaves a second reader that the table in \
             `without_a_published_plan_the_arena_has_no_ceiling_of_ours` does not judge"
        );
        assert!(
            !body.contains("unwrap_or("),
            "a default inside cuda_provider is a ceiling nobody measured on this card: that is \
             the 2048 that made the real reranker fail in the BFCArena on every machine \
             without a plan"
        );
        assert!(
            body.contains("SameAsRequested"),
            "with the extend strategy that grows by what is asked, a ceiling below what the \
             model needs saves no memory and only moves the failure into the load"
        );
        assert!(
            body.contains("error_on_failure()"),
            "dropping this turns a provider that will not start into a silent CPU session \
             again: the registration has to fail so `configure` can say so"
        );
    }

    /// What ONNX Runtime said on this machine on 2026-10-12, with the operator's
    /// name swapped for a canary. `status_after_sessions` carries it whole
    /// because `doctor` runs on the machine of the person who has to read it.
    const PROVIDER_REFUSAL: &str = r#"E:\_work\1\s\onnxruntime\core\session\provider_bridge_ort.cc:1962 onnxruntime::ProviderLibrary::Get [ONNXRuntimeError] : 1 : FAIL : Error loading "C:\Users\canaryoperator\.cache\cuba-memorys\onnxruntime\onnxruntime_providers_cuda.dll" which depends on "cublasLt64_12.dll" which is missing. (Error 126: "No se puede encontrar el módulo especificado.")"#;

    /// The eight sentences `status_from` can answer with: four machines under a
    /// build with a provider and four under one without.
    fn the_eight_rows() -> Vec<(Option<&'static str>, bool, bool)> {
        [Some("cuda"), None]
            .into_iter()
            .flat_map(|compiled| {
                [(false, false), (false, true), (true, false), (true, true)]
                    .into_iter()
                    .map(move |(runtime_gpu, device_present)| {
                        (compiled, runtime_gpu, device_present)
                    })
            })
            .collect()
    }

    /// With nothing refused the block says what it said before W1, on every row.
    ///
    /// Green on the unmodified tree on purpose: it is the guard against
    /// `status_after_sessions` touching a machine whose sessions all opened
    /// where they were asked to, which would turn every healthy daemon red.
    #[tokio::test]
    async fn with_no_refusal_the_gpu_block_says_exactly_what_it_said_before() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        for (compiled, runtime_gpu, device_present) in the_eight_rows() {
            let before = status_from(compiled, runtime_gpu, device_present);
            let after =
                status_after_sessions(status_from(compiled, runtime_gpu, device_present), &[]);
            let row =
                format!("compiled={compiled:?} runtime_gpu={runtime_gpu} device={device_present}");
            assert_eq!(after.degraded, before.degraded, "{row}");
            assert_eq!(
                after.detail, before.detail,
                "{row}: a registry with no refusal in it must not rewrite the sentence"
            );
            assert_eq!(after.hint, before.hint, "{row}");
        }
    }

    /// The case measured on this machine: the card, the runtime and the provider
    /// library are all there, `cublasLt64_12.dll` is not, and the reranker opened
    /// on the CPU. That row is the only healthy one, and it has to stop saying so.
    #[tokio::test]
    async fn a_provider_that_would_not_start_turns_the_healthy_row_degraded_and_gives_a_way_out() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let healthy = status_from(Some("cuda"), true, true);
        assert!(
            !healthy.degraded && healthy.hint.is_none(),
            "the fixture is broken: with the card, the runtime and the provider library all \
             present `status_from` is the one row that is not degraded, and without that control \
             the assertions below would pass on a row that was already red"
        );

        let after = status_after_sessions(
            status_from(Some("cuda"), true, true),
            &[(Workload::Reranker, PROVIDER_REFUSAL.to_string())],
        );

        assert!(
            after.degraded,
            "a session that asked for the card and opened on the CPU is the thing `degraded` \
             exists to say. Reported as ok, `doctor` prints a green «gpu» line on the machine \
             whose reranker takes a minute per search"
        );
        assert!(
            after.detail.contains("reranker") && after.detail.contains("CPU"),
            "the sentence names which model fell and where it landed: {}",
            after.detail
        );
        assert!(
            after.detail.contains(PROVIDER_REFUSAL),
            "and carries what ONNX Runtime said, whole: it names the DLL that is missing, and \
             that is the one thing the operator can go and fetch. `doctor` runs on their \
             machine, so there is nothing to redact: {}",
            after.detail
        );
        let hint = after.hint.unwrap_or_default();
        assert!(
            !hint.trim().is_empty(),
            "`doctor` only warns on `Some(hint) if degraded`; a degraded status with no hint is \
             printed as ok, which is the defect again one level up"
        );
        assert!(
            !hint.contains("cuba-memorys"),
            "the binary is `memory-industry`; `cuba-memorys` is the compat alias and the hint \
             is a command somebody will type: {hint}"
        );
    }

    /// Every row, one refusal and two: each refused workload is named with its
    /// own cause, the answer is degraded, and there is a hint exactly when there
    /// is a degradation.
    #[tokio::test]
    async fn every_refused_workload_is_named_with_its_own_cause_on_every_row() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let embedder_cause = "refusal-of-the-embedder-4711";
        let nli_cause = "refusal-of-the-nli-4712";
        let cases: [(&str, Vec<(Workload, String)>); 2] = [
            (
                "embedder alone",
                vec![(Workload::Embedder, embedder_cause.to_string())],
            ),
            (
                "embedder and nli",
                vec![
                    (Workload::Embedder, embedder_cause.to_string()),
                    (Workload::Nli, nli_cause.to_string()),
                ],
            ),
        ];

        for (compiled, runtime_gpu, device_present) in the_eight_rows() {
            for (which, refused) in &cases {
                let after = status_after_sessions(
                    status_from(compiled, runtime_gpu, device_present),
                    refused,
                );
                let row = format!(
                    "{which} on compiled={compiled:?} runtime_gpu={runtime_gpu} \
                     device={device_present}: {}",
                    after.detail
                );

                assert!(after.degraded, "{row}");
                assert_eq!(
                    after.hint.is_some(),
                    after.degraded,
                    "a hint exists if and only if the status is degraded (`doctor` and \
                     `every_hint_names_the_same_binary` both rely on it): {row}"
                );
                for (workload, cause) in refused {
                    assert!(
                        after.detail.contains(workload.label()),
                        "{} is refused and not named: {row}",
                        workload.label()
                    );
                    assert!(
                        after.detail.contains(cause.as_str()),
                        "{} is named without its cause: {row}",
                        workload.label()
                    );
                }
                if refused.len() == 1 {
                    assert!(
                        !after.detail.contains(nli_cause),
                        "a cause that belongs to a workload nobody refused showed up: {row}"
                    );
                }
                assert!(
                    !after.hint.unwrap_or_default().contains("cuba-memorys"),
                    "{row}"
                );
            }
        }
    }

    /// Puts one slot of the registry back the way it was found when it drops,
    /// panic or not. Without the restore in `Drop`, a red assertion leaves the
    /// refusal for every later test in this binary, and
    /// `status_reports_the_provider_this_binary_was_compiled_with` would be the
    /// one to go red over it.
    struct RestoredPlacement {
        workload: Workload,
        previous: Option<Placement>,
    }

    impl RestoredPlacement {
        fn take(workload: Workload) -> Self {
            let previous =
                PLACEMENTS.lock().unwrap_or_else(|p| p.into_inner())[slot(workload)].clone();
            Self { workload, previous }
        }
    }

    impl Drop for RestoredPlacement {
        fn drop(&mut self) {
            PLACEMENTS.lock().unwrap_or_else(|p| p.into_inner())[slot(self.workload)] =
                self.previous.take();
        }
    }

    fn forget(workload: Workload) {
        PLACEMENTS.lock().unwrap_or_else(|p| p.into_inner())[slot(workload)] = None;
    }

    /// `record`, `placement` and `status()` are one registry: what `configure`
    /// writes is what `/health` reads and what `doctor` judges.
    ///
    /// Each assertion here is the one a mutant of those three functions fails:
    /// a `placement` that answers `None`, a `record` that writes the wrong slot
    /// or does not overwrite, and a `status()` that goes back to `status_from`
    /// without the registry.
    #[tokio::test]
    async fn status_and_placement_read_the_one_registry_that_configure_writes() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let embedder = RestoredPlacement::take(Workload::Embedder);
        let reranker = RestoredPlacement::take(Workload::Reranker);
        let nli = RestoredPlacement::take(Workload::Nli);
        let found = [
            embedder.previous.clone(),
            reranker.previous.clone(),
            nli.previous.clone(),
        ];
        for workload in [Workload::Embedder, Workload::Reranker, Workload::Nli] {
            forget(workload);
        }

        let before = status();
        for workload in [Workload::Embedder, Workload::Reranker, Workload::Nli] {
            assert_eq!(
                placement(workload),
                None,
                "control: a session nobody opened has no placement, and `{}` says so with `None` \
                 rather than with a device nobody measured",
                workload.label()
            );
        }

        let nli_cause = "the nli provider refusal 4713";
        record(
            Workload::Nli,
            Placement::CpuAfterProviderFailed(nli_cause.to_string()),
        );
        record(Workload::Embedder, Placement::Cpu);
        record(Workload::Reranker, Placement::Gpu);

        assert_eq!(
            placement(Workload::Nli),
            Some(Placement::CpuAfterProviderFailed(nli_cause.to_string())),
            "what `record` wrote is what `placement` reads back, cause included"
        );
        assert_eq!(
            placement(Workload::Embedder),
            Some(Placement::Cpu),
            "one slot per workload: the embedder's answer is its own, not the last one written"
        );
        assert_eq!(placement(Workload::Reranker), Some(Placement::Gpu));

        let refused = status();
        assert!(
            refused.degraded,
            "`status()` has to read the same registry `placement()` does: one refusal in it and \
             the block is degraded, whatever this machine has installed"
        );
        assert!(
            refused.detail.contains("nli") && refused.detail.contains(nli_cause),
            "and it names the workload and the cause that were written: {}",
            refused.detail
        );
        assert!(refused.hint.is_some(), "a degraded status carries its hint");

        record(Workload::Nli, Placement::Gpu);
        assert_eq!(
            placement(Workload::Nli),
            Some(Placement::Gpu),
            "the last write wins: a session that opens again, after the DLLs were installed, \
             replaces the refusal instead of sitting behind it for the life of the process"
        );
        let healed = status();
        assert_eq!(
            (
                healed.degraded,
                healed.detail.as_str(),
                healed.hint.as_deref()
            ),
            (
                before.degraded,
                before.detail.as_str(),
                before.hint.as_deref()
            ),
            "with the refusal overwritten the block is the one it was with nothing in the \
             registry"
        );

        let embedder_cause = "the embedder provider refusal 4714";
        record(
            Workload::Embedder,
            Placement::CpuAfterProviderFailed(embedder_cause.to_string()),
        );
        record(
            Workload::Nli,
            Placement::CpuAfterProviderFailed(nli_cause.to_string()),
        );
        let both = status().detail;
        match (both.find(embedder_cause), both.find(nli_cause)) {
            (Some(first), Some(second)) => assert!(
                first < second,
                "the order is fixed — embedder, reranker, nli — so two runs of the same \
                 machine print the same sentence: {both}"
            ),
            other => panic!("both refusals have to be in the sentence, got {other:?}: {both}"),
        }

        drop(embedder);
        drop(reranker);
        drop(nli);
        let restored = status();
        assert_eq!(
            (restored.degraded, restored.detail.as_str()),
            (before.degraded, before.detail.as_str()),
            "after the restore the registry is what this test found, so the next test in the \
             binary reads the machine and not this one's refusals"
        );
        for (workload, original) in [Workload::Embedder, Workload::Reranker, Workload::Nli]
            .into_iter()
            .zip(found)
        {
            assert_eq!(
                PLACEMENTS.lock().unwrap_or_else(|p| p.into_inner())[slot(workload)],
                original,
                "{} was not put back",
                workload.label()
            );
        }
    }

    /// A commit that failed is not a session that opened, so whatever the
    /// registry held stays exactly as it was: neither overwritten by the
    /// placement that was attempted nor cleared.
    ///
    /// Green on the unmodified tree on purpose: it is the guard against
    /// `record_if_built` writing before it has looked at the result, which is
    /// the one change that would make the structural test below pass while the
    /// registry went on lying. The sentinel is a refusal because that is the
    /// one value `status()` turns into `degraded`.
    #[tokio::test]
    async fn a_commit_that_failed_leaves_the_registry_as_it_found_it() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let _restored = RestoredPlacement::take(Workload::Nli);
        let sentinel = Placement::CpuAfterProviderFailed("sentinel k3-4717".to_string());
        record(Workload::Nli, sentinel.clone());

        for attempted in [
            Placement::Gpu,
            Placement::Cpu,
            Placement::CpuAfterProviderFailed("refusal".to_string()),
        ] {
            let refused = record_if_built(
                Workload::Nli,
                attempted.clone(),
                Err::<(), _>(anyhow::anyhow!("commit refused k3-4716")),
            );
            let error = refused.expect_err("a failed commit has to come back as the error it was");
            assert_eq!(
                format!("{error:#}"),
                "commit refused k3-4716",
                "{attempted:?}: the caller reads the cause of the commit, not a rewording of it"
            );
            assert_eq!(
                placement(Workload::Nli),
                Some(sentinel.clone()),
                "{attempted:?}: a commit that failed changed the registry. Written, it reports \
                 a placement for a session that never existed; cleared, it forgets one that did"
            );
        }

        let built = record_if_built(Workload::Nli, Placement::Cpu, Ok(42u8));
        assert_eq!(
            built.expect("a commit that worked is returned as it came"),
            42,
            "the value of the commit has to come through the registry write untouched"
        );
        assert_eq!(
            placement(Workload::Nli),
            Some(Placement::Cpu),
            "control: a commit that worked is the one case that writes, and it writes what was \
             decided when the builder was configured"
        );
    }

    /// The provider refused and the CPU session did not open either: both
    /// causes have to reach whoever reads the error, in the order they
    /// happened, joined by the sentence `/health` cuts on. Pure on purpose: it
    /// never touches the registry, which a failed commit leaves as it was.
    #[test]
    fn a_commit_that_fails_after_the_provider_refused_carries_both_causes() {
        let refusal = "refusal k5-4731: \"cublasLt64_12.dll\" which is missing";
        let refused = Placement::CpuAfterProviderFailed(refusal.to_string());

        let both = after_the_refusal(
            &refused,
            anyhow::anyhow!("load model: commit refused k5-4732"),
        );
        let served = format!("{both:#}");
        assert_eq!(
            served,
            format!(
                "the GPU provider did not start: {refusal}{AND_THEN_ON_THE_CPU}load model: \
                 commit refused k5-4732"
            ),
            "the provider's refusal is lost when the commit fails: the operator fixes the commit, \
             restarts, and only then finds out the provider never started"
        );
        assert_eq!(
            served.matches(AND_THEN_ON_THE_CPU).count(),
            1,
            "`/health` cuts on this sentence: twice, it cuts the commit's cause in the wrong \
             place: {served}"
        );

        let chained = after_the_refusal(
            &refused,
            anyhow::anyhow!("inner k5-4734").context("load model: k5-4735"),
        );
        let served = format!("{chained:#}");
        let (_, tail) = served
            .split_once(AND_THEN_ON_THE_CPU)
            .expect("the separator is in the error: without it `/health` cannot tell the halves");
        assert_eq!(
            tail, "load model: k5-4735: inner k5-4734",
            "the commit's chain has to arrive whole and not only its outer line"
        );
    }

    /// Green on the unmodified tree: the guard against writing the refusal
    /// when there was none. A GPU placement that opened and a CPU one nobody
    /// asked to leave the card have nothing to say about a provider.
    #[test]
    fn a_commit_that_fails_without_a_refusal_is_the_commit_error_as_it_came() {
        for placement in [Placement::Gpu, Placement::Cpu] {
            let served = format!(
                "{:#}",
                after_the_refusal(&placement, anyhow::anyhow!("load model: k5-4736"))
            );
            assert_eq!(
                served, "load model: k5-4736",
                "{placement:?}: no provider refused, so the error is the commit's and nothing else"
            );
            assert!(
                !served.contains(AND_THEN_ON_THE_CPU),
                "{placement:?}: the separator is only for the double failure: {served}"
            );
        }
    }

    /// Production source of this file with the comment lines dropped, so a
    /// sentence that names a call cannot satisfy or break a structural check.
    fn production_without_comments() -> String {
        include_str!("gpu.rs")
            .split_once("#[cfg(test)]\nmod placement_tests")
            .expect("the test module follows the production code")
            .0
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// From `header` to the first closing brace in column 0, which is where a
    /// top-level function of this file ends.
    fn span_of(source: &str, header: &str, why: &str) -> std::ops::Range<usize> {
        let start = source.find(header).expect(why);
        let end = source[start..]
            .find("\n}")
            .map_or(source.len(), |at| start + at);
        start..end
    }

    /// Structural, like its neighbours: what makes it worth anything is that
    /// the registry is the only thing `/health` and `doctor` trust about where
    /// a session opened, and the way it lies is a write that happens before
    /// the commit has an answer. The runtime half is the K3-T6 block in
    /// `tests/v017_rerank_gpu.rs`.
    #[test]
    fn only_open_writes_the_registry_and_only_after_the_commit() {
        let production = production_without_comments();

        let open = span_of(
            &production,
            "pub fn open<",
            "`gpu::open` is the one door a session opens through and it is not in this file",
        );
        let writers: Vec<usize> = production
            .match_indices("record_if_built(")
            .map(|(at, _)| at)
            .collect();
        assert_eq!(
            writers.len(),
            1,
            "`record_if_built(` is called {} times and has to be called once, from `open`. A \
             call from `configure` writes a placement before the commit has run: a session that \
             never existed shows up in `/health` and, when it is a refusal, turns \
             `gpu.degraded` true",
            writers.len()
        );
        assert!(
            open.contains(&writers[0]),
            "the one `record_if_built(` is not inside `open`: whoever calls it from elsewhere \
             writes a placement for a session whose commit nobody has seen"
        );
        assert!(
            production[open].contains("commit("),
            "`open` does not call the commit, so what it hands to `record_if_built` is not the \
             result of opening the session"
        );

        let record_body = span_of(
            &production,
            "fn record_if_built<",
            "`record_if_built` is the one function that may call `record` and it is not here",
        );
        for (at, _) in production.match_indices("record(") {
            let before = &production[..at];
            let is_the_definition = before.ends_with("fn ");
            let is_a_longer_name = before.ends_with(|c: char| c.is_alphanumeric() || c == '_');
            if is_the_definition || is_a_longer_name {
                continue;
            }
            assert!(
                record_body.contains(&at),
                "`record(` is called outside `record_if_built`, at byte {at}: a direct write \
                 skips the check that the commit worked"
            );
        }

        assert!(
            !production.contains("pub fn configure"),
            "`configure` is public again: a caller can configure a builder and never go \
             through `open`, and the registry is back to being written by whoever remembers"
        );

        for (caller, source) in [
            ("embeddings/onnx.rs", include_str!("embeddings/onnx.rs")),
            ("search/rerank.rs", include_str!("search/rerank.rs")),
            ("cognitive/nli.rs", include_str!("cognitive/nli.rs")),
        ] {
            assert!(
                source.contains("crate::gpu::open("),
                "{caller} does not open its session through `crate::gpu::open`: its placement \
                 is written, if at all, by something that has not seen the commit"
            );
            assert!(
                !source.contains("gpu::configure("),
                "{caller} still calls `gpu::configure` directly: that is the door that wrote \
                 the registry before the commit"
            );
        }
    }

    /// Structural: `after_the_refusal` is worth nothing if nobody calls it, and
    /// `open` is the only function that still holds the placement when the
    /// commit fails. The runtime half needs ONNX Runtime and a provider that
    /// refuses, which no build the gate compiles has.
    #[test]
    fn open_hands_a_failed_commit_to_after_the_refusal() {
        let production = production_without_comments();
        let open = span_of(
            &production,
            "pub fn open<",
            "`gpu::open` is the one door a session opens through and it is not in this file",
        );
        assert!(
            production[open].contains("after_the_refusal("),
            "`open` does not call `after_the_refusal`: it is the only place that still has the \
             placement when the commit fails, and without the call the provider's refusal lives \
             only in the log again"
        );
    }

    /// Structural, and in every build: `directml_provider` only compiles with
    /// the feature, the gate never builds it alone and no machine here has a
    /// card that is not NVIDIA, so the text is all that can be held to. In the
    /// build that is published DirectML sits behind a CUDA that already
    /// opened, and there `error_on_failure` would turn a machine without
    /// `DirectML.dll` from a GPU session into a refusal.
    #[test]
    fn the_directml_provider_fails_loudly_when_it_is_the_one_that_decides() {
        let production = production_without_comments();

        let provider = span_of(
            &production,
            "fn directml_provider()",
            "directml_provider is in this file: without it the list in `configure` registers \
             DirectML as a bare provider, and ORT falls back to the CPU under a log that still \
             says GPU",
        );
        let body = &production[provider];
        assert!(
            body.contains("error_on_failure()"),
            "a DirectML registration that cannot fail is the CUDA defect again, on the machines \
             whose only provider is DirectML: a session on the CPU that every check reports as GPU"
        );
        assert!(
            body.contains("cfg!(feature = \"cuda\")"),
            "with CUDA in the build DirectML must not carry `error_on_failure`: it only \
             registers behind a CUDA that already opened, and failing there turns a working GPU \
             session into a refusal on every NVIDIA machine without DirectML.dll"
        );

        let configure = span_of(&production, "fn configure<F>(", "configure is in this file");
        let configure_body = &production[configure];
        assert!(
            configure_body.contains("directml_provider()"),
            "the provider list in `configure` does not ask `directml_provider()`"
        );
        assert!(
            !configure_body.contains("DirectML::default()"),
            "`configure` builds DirectML by hand again, and the choice about `error_on_failure` \
             is made in two places"
        );
    }
}
