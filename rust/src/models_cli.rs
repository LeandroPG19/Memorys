use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The ONNX Runtime `models runtime` installs. Public because `tests/model_parity.rs`
/// compares it with the runtime the judge loads: the vectors it pins are measured on
/// that one, and a judge on another version would be guarding a runtime nobody installs.
pub const ORT_VERSION: &str = "1.31.1";

/// Which package of the release an install came from. The CUDA major is part of the
/// name since 1.27: `gpu_cuda12` and `gpu_cuda13` do not share a cudart or a cuBLAS,
/// and what `doctor` and the hint of a provider that would not start say depends on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavor {
    Cpu,
    Cuda12,
    Cuda13,
}

impl Flavor {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Flavor::Cpu => "cpu",
            Flavor::Cuda12 => "cuda12",
            Flavor::Cuda13 => "cuda13",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Flavor::Cpu, Flavor::Cuda12, Flavor::Cuda13]
            .into_iter()
            .find(|flavor| flavor.as_str() == text)
    }

    pub(crate) fn is_gpu(self) -> bool {
        self != Flavor::Cpu
    }
}

struct ModelSpec {
    dir: &'static str,
    hf_repo: &'static str,
    files: &'static [(&'static str, &'static str)],
    env_hint: &'static str,
}

fn spec(name: &str) -> Option<ModelSpec> {
    match name {
        "embed" | "embeddings" => Some(ModelSpec {
            dir: "models",
            hf_repo: "Xenova/multilingual-e5-small",
            files: &[
                ("onnx/model_quantized.onnx", "model_quantized.onnx"),
                ("tokenizer.json", "tokenizer.json"),
            ],
            env_hint: "ONNX_MODEL_PATH",
        }),
        "nli" => Some(ModelSpec {
            dir: "models-nli",
            hf_repo: "MoritzLaurer/mDeBERTa-v3-base-xnli-multilingual-nli-2mil7",
            files: &[
                ("onnx/model.onnx", "model.onnx"),
                ("tokenizer.json", "tokenizer.json"),
                ("config.json", "config.json"),
                ("tokenizer_config.json", "tokenizer_config.json"),
            ],
            env_hint: "CUBA_NLI_PATH",
        }),
        "reranker" => Some(ModelSpec {
            dir: "reranker",
            hf_repo: "celinehoang/bge-reranker-v2-m3-onnx",
            files: &[
                // Hub split the former monolithic ~1.1 GB graph: tiny model.onnx +
                // external weights in model.onnx_data (ONNX external-data format).
                ("model.onnx", "model.onnx"),
                ("model.onnx_data", "model.onnx_data"),
                ("tokenizer.json", "tokenizer.json"),
                ("config.json", "config.json"),
            ],
            env_hint: "CUBA_RERANKER_PATH",
        }),
        _ => None,
    }
}

/// The two cache roots: the documented one, and the one every install from
/// before the rename wrote to. The legacy root is still read for one release
/// (AGENTS.md); `cache migrate` is how an install stops depending on that.
pub(crate) struct CacheRoots {
    pub(crate) preferred: PathBuf,
    pub(crate) legacy: PathBuf,
}

impl CacheRoots {
    pub(crate) fn under(home: &Path) -> Self {
        let cache = home.join(".cache");
        Self {
            preferred: cache.join("memory-industry"),
            legacy: cache.join("cuba-memorys"),
        }
    }

    pub(crate) fn resolve() -> Result<Self> {
        // The outer context stays. `envs::home()` names both variables and talks
        // about where an MCP client looks for a config, which does not say WHAT
        // could not be located here; without this line the operator gets an error
        // that only blames the environment.
        let home = crate::envs::home().context("no sé dónde está la caché")?;
        Ok(Self::under(&home))
    }
}

fn cache_root() -> Result<PathBuf> {
    let CacheRoots { preferred, legacy } = CacheRoots::resolve()?;
    if preferred.exists() || !legacy.exists() {
        Ok(preferred)
    } else {
        Ok(legacy)
    }
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    let what = args.first().map(String::as_str).unwrap_or("");
    match what {
        "embed" | "embeddings" | "nli" | "reranker" => {
            download_model(&spec(what).unwrap()).await?;
        }
        "runtime" => {
            download_runtime(&parse_runtime_args(&args[1..])?).await?;
        }
        "all" => {
            download_runtime(&parse_runtime_args(&args[1..])?).await?;
            for m in ["embed", "nli", "reranker"] {
                download_model(&spec(m).unwrap()).await?;
            }
            println!(
                "\nChat model (extract/judge) is separate — set it in one line:\n\
                   memory-industry llm set ollama\n\
                   memory-industry llm set deepseek --key YOUR_KEY\n\
                   memory-industry llm list"
            );
        }
        "llm" => {
            // Discoverability: people already know `models all`.
            crate::llm_cli::run_cli(&args[1..]).await?;
        }
        "" | "-h" | "--help" | "help" => {
            print_help();
        }
        other => {
            bail!(
                "modelo desconocido `{other}`. Usá: embed | nli | reranker | runtime | all | llm\n\
                 (`memory-industry models help` para más detalle)"
            );
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "memory-industry models <embed|nli|reranker|runtime|all|llm>\n\n\
         Descarga los modelos ONNX y el runtime a ~/.cache/memory-industry/ (o la caché\n\
         legado ~/.cache/cuba-memorys/ si ya existe), en cualquier sistema.\n\
         Para pasar la caché legado a la nueva: memory-industry cache migrate --apply\n\
         MemoryIndustry los encuentra ahí solo — no hace falta setear env vars.\n\n\
           embed      multilingual-e5-small (384-d) — búsqueda semántica. ~113 MB\n\
           nli        mDeBERTa-v3-xnli — verify sin LLM. ~1.1 GB\n\
           reranker   bge-reranker-v2-m3 — reordena candidatos. ~4.5 GB (onnx + onnx_data)\n\
           runtime    libonnxruntime para tu plataforma. ~11 MB (CPU), 245-450 MB con --gpu\n\
                      --gpu          el runtime con CUDA; elige 12 o 13 leyendo el driver y la tarjeta\n\
                      --cuda 12|13   fuerza la versión de CUDA (solo con --gpu)\n\
                      --if-installed solo actualiza un runtime que ya está; sin ninguno no baja nada\n\
           all        el runtime + los tres modelos\n\
           llm        chat model for extract/judge — alias of `memory-industry llm`\n\n\
         Chat (DeepSeek/Qwen/Ollama/…):  memory-industry llm set <provider> [--key KEY]\n\
         Verificá con:  memory-industry doctor"
    );
}

const KNOWN_DIGESTS: &[(&str, &str)] = &[
    (
        "reranker/model.onnx",
        "77f102be02885c81e5064485c5358e87721856d15ba750ae893480d39903a461",
    ),
    (
        "reranker/model.onnx_data",
        "c75282ab65d3f53c20a45f637fafbeb7f064cbd6b7c9df01bd8a32e41abe879b",
    ),
    (
        "reranker/tokenizer.json",
        "8bf8afbfd11306bd872018c53bfdf2e160a56f8edbcf49933324404791c148d3",
    ),
    (
        "models-bge-m3/tokenizer.json",
        "6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790",
    ),
    // ONNX Runtime v1.31.1, the `digest` GitHub shows for each asset of the release
    // (`gh api repos/microsoft/onnxruntime/releases/tags/v1.31.1`). Checked before an
    // archive is extracted; a package with no line here is refused, not trusted.
    (
        "onnxruntime/onnxruntime-linux-x64-1.31.1.tgz",
        "c849e36a1ef3e22d4ff217b9d1ce4a3cf5b9467b975610913ae959ad5b16bdc9",
    ),
    (
        "onnxruntime/onnxruntime-linux-x64-gpu_cuda12-1.31.1.tgz",
        "57cd51f677eca19a269cdba31b0c16c15d73a8feea41f170aa8b750447ab1f70",
    ),
    (
        "onnxruntime/onnxruntime-linux-x64-gpu_cuda13-1.31.1.tgz",
        "b2146862e691f7e338f5aff9c7a9a5567c2ca57cf01442a1adb3a13bf62d9095",
    ),
    (
        "onnxruntime/onnxruntime-linux-aarch64-1.31.1.tgz",
        "5317505e2da99be65f8e2775ba3c2afd6d2961fd78aba1bd8f56159f48820c78",
    ),
    (
        "onnxruntime/onnxruntime-osx-arm64-1.31.1.tgz",
        "2e1344a1926ee50eb25b38153cb6992c3567515aef0e677a1a520a1798896d6f",
    ),
    (
        "onnxruntime/onnxruntime-win-x64-1.31.1.zip",
        "5869e26bb25314375f2d5ef1877e3ea06b1543d6c710e6e4a1ec2f7177ab4b2c",
    ),
    (
        "onnxruntime/onnxruntime-win-x64-gpu_cuda12-1.31.1.zip",
        "5cbf65edc93dc7bb3296249b1b6e1b2228e575902af64d6163d0da2103cef9d5",
    ),
    (
        "onnxruntime/onnxruntime-win-x64-gpu_cuda13-1.31.1.zip",
        "6c64508de9c04f9d938297287e9d81fa47a2fbbb30cc04e6fac0db88f6cb68c1",
    ),
];

/// `sha2` 0.11 hashers no longer implement `io::Write` (`digest` 0.11 is `no_std`).
/// `io::copy` already retries `Interrupted` and handles short reads, so the hasher
/// is fed through a writer instead of a read loop of our own.
struct HashWriter<'a>(&'a mut sha2::Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn sha256_file(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut HashWriter(&mut hasher))?;
    Ok(hex::encode(hasher.finalize()))
}

pub fn expected_digest(key: &str) -> Option<&'static str> {
    KNOWN_DIGESTS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, digest)| *digest)
}

fn verify_checksum(dest: &std::path::Path, key: &str) -> Result<()> {
    let Some(expected) = expected_digest(key) else {
        println!("  (sin checksum registrado para {key} — no verificado)");
        return Ok(());
    };
    let actual = sha256_file(dest)?;
    if actual != expected {
        std::fs::remove_file(dest).ok();
        anyhow::bail!(
            "checksum mismatch for {key}: expected {expected}, got {actual}. \
             The file was deleted — the download was tampered with or corrupted."
        );
    }
    println!("  ✓ checksum");
    Ok(())
}

fn has_content(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() > 0)
}

async fn download_model(spec: &ModelSpec) -> Result<()> {
    let dir = cache_root()?.join(spec.dir);

    println!(
        "→ {} ({} archivos) en {}",
        spec.hf_repo,
        spec.files.len(),
        dir.display()
    );
    let installed = dir.clone();
    materialize_dir(&dir, move |staging| async move {
        for (remote, local) in spec.files {
            if has_content(&installed.join(local)) {
                println!("  ✓ {local} (ya está)");
                continue;
            }
            let dest = staging.join(local);
            if has_content(&dest) {
                print!("  ✓ {local} (bajado en un intento anterior) ... ");
            } else {
                let url = format!(
                    "https://huggingface.co/{}/resolve/main/{remote}",
                    spec.hf_repo
                );
                print!("  ↓ {local} ... ");
                std::io::stdout().flush().ok();
                download_to(&url, &dest)
                    .await
                    .with_context(|| format!("descargando {local}"))?;
            }
            verify_checksum(&dest, &format!("{}/{}", spec.dir, local))?;
            let mb = std::fs::metadata(&dest)?.len() / 1_048_576;
            println!("{mb} MB");
        }
        Ok::<_, anyhow::Error>(())
    })
    .await?;
    println!(
        "  listo. MemoryIndustry lo encuentra en {} (o {}=<ruta>)",
        dir.display(),
        spec.env_hint
    );
    Ok(())
}

/// The package of the release for a platform and a flavor: its name without the
/// extension, the extension, and the library the loader looks for.
///
/// `os` and `arch` come in as text so one machine can answer for every platform; the
/// caller hands it `std::env::consts`. The GPU packages exist for x64 Linux and
/// Windows only, and there is no macOS x86_64 package after 1.23.2: both are an error
/// that says so, because the alternative was a 404 on a name that looked right and,
/// for the GPU, a CPU runtime handed over in silence.
fn runtime_archive(
    os: &str,
    arch: &str,
    flavor: Flavor,
) -> Result<(String, &'static str, &'static str)> {
    let v = ORT_VERSION;
    let (platform, ext, lib) = match (os, arch) {
        ("linux", "x86_64") => ("linux-x64", "tgz", "libonnxruntime.so"),
        ("linux", "aarch64") => ("linux-aarch64", "tgz", "libonnxruntime.so"),
        ("macos", "aarch64") => ("osx-arm64", "tgz", "libonnxruntime.dylib"),
        ("windows", "x86_64") => ("win-x64", "zip", "onnxruntime.dll"),
        ("macos", "x86_64") => bail!(
            "no hay paquete de ONNX Runtime para macos/x86_64 desde la 1.24 (el último es la \
             1.23.2) y este binario pide la 1.24 o más. Compilá uno y apuntá ORT_DYLIB_PATH a la \
             librería."
        ),
        _ => bail!(
            "no tengo el runtime prearmado para {os}/{arch}. Instalá onnxruntime {v} a mano \
             y apuntá ORT_DYLIB_PATH a la librería."
        ),
    };
    let suffix = match flavor {
        Flavor::Cpu => "",
        Flavor::Cuda12 | Flavor::Cuda13 if !matches!(platform, "linux-x64" | "win-x64") => bail!(
            "Microsoft no publica runtime GPU de núcleo para {os}-{arch}; sin --gpu baja el de CPU"
        ),
        Flavor::Cuda12 => "-gpu_cuda12",
        Flavor::Cuda13 => "-gpu_cuda13",
    };
    Ok((format!("onnxruntime-{platform}{suffix}-{v}"), ext, lib))
}

/// What the CUDA driver says about this machine, read the way `nvidia-smi` does
/// but through `libcuda`: `cuDriverGetVersion` (12080 is CUDA 12.8) and the
/// compute capability of card 0. The version is read first and stands alone: a
/// card that does not answer (`compute: None`) is still a driver with a version.
struct CudaProbe {
    driver: i32,
    compute: Option<(i32, i32)>,
}

/// Turing, SM 7.5: the oldest card the CUDA 13 packages are built for.
const OLDEST_CARD_OF_CUDA_13: (i32, i32) = (7, 5);

fn cuda_version(driver: i32) -> String {
    format!("{}.{}", driver / 1000, driver % 1000 / 10)
}

/// Which GPU package to fetch: the flag when there is one, and otherwise what the
/// driver and the card can run. Every row of the table is judged on a machine with
/// no card because the reading comes in as `probe`, and it is called only when its
/// answer decides: `--cuda 12` never reads the driver, and `cuInit` on a machine
/// with a card is not free. CUDA 12 is deprecated by Microsoft since 1.27, so the
/// answer is 13 unless the driver or the card cannot take it. No driver at all is
/// 13 with a line that says so: an install made ahead of the card is the common
/// case, and the line tells how to ask for 12.
fn choose_cuda(
    forced: Option<u8>,
    probe: &dyn Fn() -> Option<CudaProbe>,
) -> Result<(Flavor, Option<String>)> {
    match forced {
        Some(12) => Ok((Flavor::Cuda12, None)),
        Some(_) => forced_cuda_13(probe()),
        None => cuda_the_driver_can_run(probe()),
    }
}

/// `--cuda 13` holds against everything but a card that cannot run it.
fn forced_cuda_13(probe: Option<CudaProbe>) -> Result<(Flavor, Option<String>)> {
    match probe.and_then(|card| card.compute) {
        Some(compute) if compute < OLDEST_CARD_OF_CUDA_13 => bail!(
            "CUDA 13 no soporta la tarjeta 0 (capacidad de cómputo {}.{}, anterior a Turing): \
             usá --cuda 12",
            compute.0,
            compute.1
        ),
        _ => Ok((Flavor::Cuda13, None)),
    }
}

/// The version of the driver decides below 13 without looking at the card; from 13 up
/// the card decides, and a card that did not answer is CUDA 13 with a line that says so.
fn cuda_the_driver_can_run(probe: Option<CudaProbe>) -> Result<(Flavor, Option<String>)> {
    let Some(card) = probe else {
        return Ok((
            Flavor::Cuda13,
            Some(
                "no veo driver NVIDIA: bajo CUDA 13; con un driver de CUDA 12 o una tarjeta \
                 anterior a Turing (SM < 7.5) usá --cuda 12"
                    .to_string(),
            ),
        ));
    };
    if card.driver < 12000 {
        bail!(
            "el driver soporta CUDA {}; el runtime GPU necesita 12 o 13: actualizá el driver o \
             bajá el de CPU sin --gpu",
            cuda_version(card.driver)
        );
    }
    if card.driver < 13000 {
        return Ok((Flavor::Cuda12, None));
    }
    Ok(match card.compute {
        None => (
            Flavor::Cuda13,
            Some(
                "no pude leer la tarjeta 0: bajo CUDA 13; con una tarjeta anterior a Turing \
                 (SM < 7.5) usá --cuda 12"
                    .to_string(),
            ),
        ),
        Some(compute) if compute < OLDEST_CARD_OF_CUDA_13 => (
            Flavor::Cuda12,
            Some(format!(
                "CUDA 13 no soporta SM {}.{}: bajo CUDA 12",
                compute.0, compute.1
            )),
        ),
        Some(_) => (Flavor::Cuda13, None),
    })
}

/// `libcuda`, loaded and asked, or `None` when there is no driver. The card is
/// asked after the version and may not answer: that is a driver with no capability,
/// not a machine with no driver. `cuDriverGetVersion` needs no `cuInit`; the
/// capability does.
fn probe_cuda() -> Option<CudaProbe> {
    let name = if cfg!(windows) {
        "nvcuda.dll"
    } else if cfg!(target_os = "linux") {
        "libcuda.so.1"
    } else {
        return None;
    };
    // SAFETY: loading the driver runs its initialisation and nothing of ours; a name
    // that is not there is an `Err`, which is the answer «no driver».
    let library = unsafe { libloading::Library::new(name) }.ok()?;
    let probe = read_cuda(&library);
    // The driver does not support being unloaded after `cuInit`; the process is a
    // command that ends right after.
    std::mem::forget(library);
    probe
}

fn read_cuda(library: &libloading::Library) -> Option<CudaProbe> {
    type DriverVersion = unsafe extern "system" fn(*mut i32) -> i32;
    type Init = unsafe extern "system" fn(u32) -> i32;
    type DeviceGet = unsafe extern "system" fn(*mut i32, i32) -> i32;
    type Attribute = unsafe extern "system" fn(*mut i32, i32, i32) -> i32;
    const COMPUTE_CAPABILITY_MAJOR: i32 = 75;
    const COMPUTE_CAPABILITY_MINOR: i32 = 76;

    // SAFETY: the names and signatures are those of the CUDA driver API (`cuda.h`):
    // `CUresult cuDriverGetVersion(int*)`, `cuInit(unsigned int)`, `cuDeviceGet(CUdevice*,
    // int)` and `cuDeviceGetAttribute(int*, CUdevice_attribute, CUdevice)`, where
    // `CUdevice` is an `int`. Each takes pointers to locals that outlive the call, and
    // a nonzero `CUresult` is read as «no answer».
    unsafe {
        let version = library.get::<DriverVersion>(b"cuDriverGetVersion").ok()?;
        let init = library.get::<Init>(b"cuInit").ok()?;
        let device_get = library.get::<DeviceGet>(b"cuDeviceGet").ok()?;
        let attribute = library.get::<Attribute>(b"cuDeviceGetAttribute").ok()?;
        let (mut driver, mut device, mut major, mut minor) = (0, 0, 0, 0);
        if version(&mut driver) != 0 {
            return None;
        }
        let card_answered = init(0) == 0
            && device_get(&mut device, 0) == 0
            && attribute(&mut major, COMPUTE_CAPABILITY_MAJOR, device) == 0
            && attribute(&mut minor, COMPUTE_CAPABILITY_MINOR, device) == 0;
        Some(CudaProbe {
            driver,
            compute: card_answered.then_some((major, minor)),
        })
    }
}

/// The record `models runtime` writes beside the library: which package the
/// directory holds. «The library exists» does not say which version it is, and
/// loading it to find out would keep a DLL open in the process that is about to
/// replace it.
const RUNTIME_RECORD: &str = "runtime.json";

pub(crate) struct RuntimeRecord {
    pub(crate) version: String,
    pub(crate) flavor: Flavor,
    archive: String,
}

/// `None` for a record that is not there, does not parse or lacks a field: an
/// unreadable record is no record, and must not stop a reinstall.
pub(crate) fn read_runtime_record(dir: &Path) -> Option<RuntimeRecord> {
    let text = std::fs::read_to_string(dir.join(RUNTIME_RECORD)).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let field = |name: &str| json.get(name)?.as_str().map(str::to_string);
    Some(RuntimeRecord {
        version: field("version")?,
        flavor: Flavor::parse(&field("flavor")?)?,
        archive: field("archive")?,
    })
}

/// What a runtime directory holds, as far as it can be told without loading it.
enum Installed {
    Nothing,
    Recorded {
        version: String,
        flavor: Flavor,
    },
    /// Every install made before the record existed: a library, and a CUDA
    /// provider beside it or not.
    Unrecorded {
        gpu_provider: bool,
    },
}

fn installed_runtime(dir: &Path, lib_name: &str) -> Installed {
    if !has_content(&dir.join(lib_name)) {
        return Installed::Nothing;
    }
    if let Some(record) = read_runtime_record(dir) {
        return Installed::Recorded {
            version: record.version,
            flavor: record.flavor,
        };
    }
    let gpu_provider = std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains("_providers_cuda")
        })
    });
    Installed::Unrecorded { gpu_provider }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeAction {
    Keep,
    Install(Flavor),
}

/// What `models runtime` does with the directory it finds. `gpu` is the flavor that
/// `--gpu` resolved to; the caller resolves it also for an install with no record
/// that holds a CUDA provider, which is a GPU runtime of an unknown major.
///
/// The directory stays only when its record names the pinned version and no other
/// flavor is asked for. Anything else is replaced, with the flavor asked for, or
/// else the one it already had.
fn runtime_action(installed: &Installed, pinned: &str, gpu: Option<Flavor>) -> RuntimeAction {
    match (installed, gpu) {
        (Installed::Recorded { version, flavor }, wanted) if version == pinned => match wanted {
            Some(other) if other != *flavor => RuntimeAction::Install(other),
            _ => RuntimeAction::Keep,
        },
        (Installed::Recorded { flavor, .. }, None) => RuntimeAction::Install(*flavor),
        (_, Some(flavor)) => RuntimeAction::Install(flavor),
        (_, None) => RuntimeAction::Install(Flavor::Cpu),
    }
}

/// What `models runtime` was asked for.
#[derive(Debug, Default, PartialEq, Eq)]
struct RuntimeArgs {
    gpu: bool,
    cuda: Option<u8>,
    if_installed: bool,
}

fn parse_runtime_args(args: &[String]) -> Result<RuntimeArgs> {
    let mut ask = RuntimeArgs::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--gpu" => ask.gpu = true,
            "--if-installed" => ask.if_installed = true,
            "--cuda" => {
                ask.cuda = Some(match rest.next().map(String::as_str) {
                    Some("12") => 12,
                    Some("13") => 13,
                    other => bail!("--cuda pide 12 o 13 (recibió {})", other.unwrap_or("nada")),
                });
            }
            other => bail!(
                "opción desconocida `{other}` (`memory-industry models help` dice cuáles hay)"
            ),
        }
    }
    if ask.cuda.is_some() && !ask.gpu {
        bail!("--cuda elige qué runtime de GPU bajar y solo va con --gpu");
    }
    Ok(ask)
}

/// A package is deleted, and nothing of it extracted, when its sum is not the
/// pinned one. A package with no pinned sum is refused as well: «no checksum
/// registered» is acceptable for a model and not for a library the process loads.
/// The table is handed in, so a test needs no real package.
fn verify_pinned(table: &[(&str, &str)], dest: &Path, key: &str) -> Result<()> {
    let Some((_, expected)) = table.iter().find(|(name, _)| *name == key) else {
        std::fs::remove_file(dest).ok();
        bail!(
            "no hay suma fijada para {key}: el runtime no se extrae sin comprobarla. \
             El archivo se borró."
        );
    };
    let actual = sha256_file(dest)?;
    if actual != *expected {
        std::fs::remove_file(dest).ok();
        bail!(
            "checksum mismatch for {key}: expected {expected}, got {actual}. \
             The file was deleted — the download was tampered with or corrupted."
        );
    }
    println!("  ✓ checksum");
    Ok(())
}

/// Checks the package against its pinned sum and only then extracts it.
fn unpack_runtime(
    table: &[(&str, &str)],
    archive: &Path,
    key: &str,
    ext: &str,
    lib_name: &str,
    dir: &Path,
    gpu: bool,
) -> Result<Vec<String>> {
    verify_pinned(table, archive, key)?;
    print!("  ⇢ extrayendo ... ");
    std::io::stdout().flush().ok();
    extract_runtime(archive, ext, lib_name, dir, gpu)
        .with_context(|| format!("extrayendo el runtime de {key}"))
}

/// The libraries the provider of each CUDA major loads, by their names on Windows
/// and on Linux. Without a flavor, the two sets: an install that predates the
/// record cannot say which one it needs. cuDNN is not in them: from 1.28 it is
/// optional at run time.
pub(crate) fn cuda_libraries(flavor: Option<Flavor>) -> String {
    let set = |major: u8| {
        format!(
            "CUDA {major}: cudart64_{major}, cublas64_{major}, cublasLt64_{major} \
             (libcudart.so.{major}, libcublas.so.{major}, libcublasLt.so.{major})"
        )
    };
    match flavor {
        Some(Flavor::Cuda12) => set(12),
        Some(Flavor::Cuda13) => set(13),
        Some(Flavor::Cpu) | None => format!("{} o {}", set(12), set(13)),
    }
}

/// `<leaf>.downloading` is a package that came whole: its record is written last,
/// after the sum was checked and the library extracted, so a directory with one is
/// the install of a run that only failed to swap it in.
fn staged_is_complete(staging: &Path, file: &str, flavor: Flavor, lib_name: &str) -> bool {
    has_content(&staging.join(lib_name))
        && read_runtime_record(staging).is_some_and(|record| {
            record.version == ORT_VERSION && record.flavor == flavor && record.archive == file
        })
}

/// Downloads, checks and extracts the package into an empty `staging`, and writes the
/// record last.
async fn fetch_runtime(
    staging: &Path,
    stem: &str,
    ext: &str,
    lib_name: &str,
    flavor: Flavor,
) -> Result<()> {
    if std::fs::symlink_metadata(staging).is_ok() {
        remove_entry(staging)?;
    }
    std::fs::create_dir_all(staging).with_context(|| format!("creando {}", staging.display()))?;
    let file = format!("{stem}.{ext}");
    let key = format!("onnxruntime/{file}");
    let url =
        format!("https://github.com/microsoft/onnxruntime/releases/download/v{ORT_VERSION}/{file}");
    let archive = staging.join(&file);
    print!("  ↓ descargando {url} ... ");
    std::io::stdout().flush().ok();
    download_to(&url, &archive)
        .await
        .context("descargando el runtime")?;
    println!("{} MB", std::fs::metadata(&archive)?.len() / 1_048_576);

    let extracted = unpack_runtime(
        KNOWN_DIGESTS,
        &archive,
        &key,
        ext,
        lib_name,
        staging,
        flavor.is_gpu(),
    )?;
    std::fs::remove_file(&archive).ok();
    println!("ok ({} librerías)", extracted.len());
    for name in &extracted {
        println!("     {name}");
    }
    let record = serde_json::json!({
        "version": ORT_VERSION,
        "flavor": flavor.as_str(),
        "archive": file,
        "sha256": expected_digest(&key),
    });
    std::fs::write(staging.join(RUNTIME_RECORD), record.to_string())
        .context("escribiendo runtime.json")?;
    Ok(())
}

/// The runtime of `staging` takes the place of `leaf` in two renames of the same
/// parent directory, so there is no moment with files of two versions in one
/// directory, and the old one is kept aside until the new one has loaded.
///
/// `rename` is handed in, as in `relocate_with`: on Linux a directory with a library
/// mapped renames without complaint, and «in use» is something only Windows says.
///
/// A process can die between the two renames, so what each directory holds on
/// entry is read before anything is deleted (`<leaf>` is the leaf, `.previous` the
/// old runtime aside, `.failed` a package that did not load):
/// - no leaf and a `.previous`: the cut. The old runtime is the only copy there is
///   and it comes back (`recover_cut_swap`); nothing is deleted before that;
/// - a leaf and a `.previous`: a swap that went through and did not finish retiring
///   the old one. What the operator put in it moves to the leaf, and the swap does
///   not go on while something stays (`retire_previous`);
/// - a leaf and a `.failed`: deleted, nothing reads it;
/// - no leaf, no `.previous`: a first install, and a `.failed` is left alone.
///
/// What stays on each failure:
/// - the leaf is in use: nothing moved, the staging stays whole for the next run;
/// - the new one does not load (`verify`): it is kept as `<leaf>.failed` and the old
///   one comes back as the leaf. With no old one (a first install) there is no leaf.
fn swap_runtime(
    staging: &Path,
    leaf: &Path,
    verify: &dyn Fn(&Path) -> Result<()>,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    let previous = sibling(leaf, "previous");
    let failed = sibling(leaf, "failed");
    recover_cut_swap(leaf, rename)?;
    set_aside(leaf, &previous, &failed, rename)?;
    if let Err(e) = rename(staging, leaf) {
        let cause = in_use_or_itself(anyhow::Error::new(e).context(format!(
            "moviendo {} a {}",
            staging.display(),
            leaf.display()
        )));
        return Err(restored(cause, &previous, leaf, rename));
    }
    if let Err(cause) = verify(leaf) {
        if rename(leaf, &failed).is_err() {
            let _ = std::fs::remove_dir_all(leaf);
        }
        let cause = cause.context("el runtime nuevo no pasó la comprobación y quedó el anterior");
        return Err(restored(cause, &previous, leaf, rename));
    }
    if let Some(notice) = retire_previous(&previous, leaf) {
        println!("  ({notice})");
    }
    Ok(())
}

/// A swap cut between its two renames leaves no leaf and the old runtime as
/// `<leaf>.previous`, the only copy there is: it goes back to its place, and the
/// line says so. With a leaf, or with nothing aside, there is nothing to undo.
fn recover_cut_swap(
    leaf: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    let previous = sibling(leaf, "previous");
    if std::fs::symlink_metadata(leaf).is_ok() || std::fs::symlink_metadata(&previous).is_err() {
        return Ok(());
    }
    rename(&previous, leaf).map_err(|e| {
        in_use_or_itself(anyhow::Error::new(e).context(format!(
            "devolviendo {} a {}",
            previous.display(),
            leaf.display()
        )))
    })?;
    println!(
        "→ runtime: el runtime anterior vuelve a su sitio ({}): un reemplazo se cortó a medias",
        leaf.display()
    );
    Ok(())
}

/// The first rename of the swap: the leaf, if there is one, goes aside as
/// `.previous`, after the leftovers of an earlier swap are dealt with (a failed
/// package is deleted, and an old runtime aside is retired, which can refuse).
fn set_aside(
    leaf: &Path,
    previous: &Path,
    failed: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    if std::fs::symlink_metadata(leaf).is_err() {
        return Ok(());
    }
    if std::fs::symlink_metadata(failed).is_ok() {
        remove_entry(failed).map_err(in_use_or_itself)?;
    }
    if let Some(notice) = retire_previous(previous, leaf) {
        bail!("un reemplazo anterior dejó el runtime viejo aparte: {notice}");
    }
    rename(leaf, previous).map_err(|e| {
        in_use_or_itself(anyhow::Error::new(e).context(format!(
            "apartando {} como {}",
            leaf.display(),
            previous.display()
        )))
    })
}

/// What is not of the package (`runtime.json`, `onnxruntime*`, `libonnxruntime*`:
/// the core and the providers of every system) is not the old runtime's to take
/// with it: the hint of a provider that would not start tells the operator to put
/// the CUDA libraries beside the runtime, and `models runtime` must not undo that.
fn belongs_to_the_package(name: &str) -> bool {
    name == RUNTIME_RECORD || name.starts_with("onnxruntime") || name.starts_with("libonnxruntime")
}

/// What of `previous` is not of the package, moved into `leaf`; the names that
/// could not go (one with that name is already there, or the rename failed).
fn carry_over(previous: &Path, leaf: &Path) -> std::io::Result<Vec<String>> {
    let mut stayed = Vec::new();
    for entry in std::fs::read_dir(previous)? {
        let name = entry?.file_name();
        let text = name.to_string_lossy().into_owned();
        if belongs_to_the_package(&text) {
            continue;
        }
        let to = leaf.join(&name);
        if std::fs::symlink_metadata(&to).is_ok()
            || std::fs::rename(previous.join(&name), &to).is_err()
        {
            stayed.push(text);
        }
    }
    Ok(stayed)
}

/// The old runtime goes once the new one loaded, with what the operator put beside
/// it moved to the new one first. `None` when `previous` is gone (or was never
/// there); otherwise what remains and where, because a file of the operator is never
/// deleted with the old package and the next run tries again.
fn retire_previous(previous: &Path, leaf: &Path) -> Option<String> {
    std::fs::symlink_metadata(previous).ok()?;
    match carry_over(previous, leaf) {
        Err(e) => Some(format!(
            "no pude leer {}: {e}; la siguiente corrida lo vuelve a intentar",
            previous.display()
        )),
        Ok(stayed) if !stayed.is_empty() => Some(format!(
            "quedan en {}: {}. La siguiente corrida los vuelve a intentar; si siguen ahí, \
             llevalos a mano a {} o borrá esa carpeta",
            previous.display(),
            stayed.join(", "),
            leaf.display()
        )),
        Ok(_) => remove_entry(previous).err().map(|e| {
            format!(
                "no pude borrar el runtime anterior, {}: la siguiente corrida lo vuelve a intentar",
                skip_reason(&e)
            )
        }),
    }
}

fn in_use_or_itself(error: anyhow::Error) -> anyhow::Error {
    if in_use(&error) {
        anyhow::anyhow!(IN_USE)
    } else {
        error
    }
}

/// The old runtime back where it was, and `cause` as the answer. A machine left
/// without a runtime is worse than the failure that put it there, so a failure to
/// restore is said in the same error, together with where the old one is.
fn restored(
    cause: anyhow::Error,
    previous: &Path,
    leaf: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> anyhow::Error {
    if std::fs::symlink_metadata(previous).is_err() {
        return cause;
    }
    match rename(previous, leaf) {
        Ok(()) => cause,
        Err(e) => cause.context(format!(
            "y no pude devolver el runtime anterior ({e}); sigue en {}",
            previous.display()
        )),
    }
}

/// The new runtime says it is the pinned version. Loaded through the same reading as
/// the daemon's, and dropped before anything else touches the directory.
fn loads_the_pinned_runtime(leaf: &Path, lib_name: &str) -> Result<()> {
    let found = crate::embeddings::onnx::runtime_version(&leaf.join(lib_name))?;
    anyhow::ensure!(
        found == ORT_VERSION,
        "el runtime dice ser la versión {found} y se esperaba la {ORT_VERSION}"
    );
    Ok(())
}

/// `--gpu` resolves to a flavor, and so does an install that has no record and a
/// CUDA provider beside the library, which is a GPU runtime the CPU package would
/// silently replace. That one stays on CUDA 12 without asking the driver: every
/// install without a record is older than the record, the installer of then only
/// fetched `…-gpu-<v>`, which is CUDA 12, and the libraries beside it are the
/// ones of 12. Moving it to 13 is what `--gpu` is for.
fn resolve_gpu(
    ask: &RuntimeArgs,
    installed: &Installed,
    probe: &dyn Fn() -> Option<CudaProbe>,
) -> Result<Option<Flavor>> {
    if ask.gpu {
        let (flavor, note) = choose_cuda(ask.cuda, probe)?;
        if let Some(note) = note {
            println!("  {note}");
        }
        return Ok(Some(flavor));
    }
    if matches!(installed, Installed::Unrecorded { gpu_provider: true }) {
        println!(
            "  runtime GPU sin registro (de antes de la 0.31): sigue en CUDA 12; \
             `models runtime --gpu` elige por el driver"
        );
        return Ok(Some(Flavor::Cuda12));
    }
    Ok(None)
}

/// Two `models runtime` at once share one staging directory and one leaf, and
/// the second would delete what the first is filling. One lock file in the cache
/// root, taken without waiting: an `npm install` does not hang behind another run.
const RUNTIME_LOCK: &str = "onnxruntime.lock";

/// The lock, held for as long as the returned file lives, or `None` for a cache
/// root that is not there when all that was asked is to update what exists: it
/// has no runtime and no `.previous`, and creating it would change which root
/// `gpu::runtime_dir` and `locate_onnxruntime` choose.
fn lock_runtime(root: &Path, if_installed: bool) -> Result<Option<std::fs::File>> {
    if !root.exists() {
        if if_installed {
            return Ok(None);
        }
        std::fs::create_dir_all(root).with_context(|| format!("creando {}", root.display()))?;
    }
    take_lock(&root.join(RUNTIME_LOCK)).map(Some)
}

fn take_lock(path: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("abriendo {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => bail!(
            "otro `memory-industry models runtime` está cambiando el runtime (candado {}): \
             esperá a que termine y volvé a correr",
            path.display()
        ),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("tomando el candado {}", path.display()))
        }
    }
}

fn nothing_to_update() {
    println!(
        "→ runtime: no hay ninguno instalado; `memory-industry models runtime` baja el primero"
    );
}

async fn download_runtime(ask: &RuntimeArgs) -> Result<()> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let (_, _, lib_name) = runtime_archive(os, arch, Flavor::Cpu)?;
    let root = cache_root()?;
    let Some(_lock) = lock_runtime(&root, ask.if_installed)? else {
        nothing_to_update();
        return Ok(());
    };
    let dir = root.join("onnxruntime");
    let installed = installed_after_a_cut_swap(&dir, lib_name)?;

    if ask.if_installed && matches!(installed, Installed::Nothing) {
        nothing_to_update();
        return Ok(());
    }
    let gpu = resolve_gpu(ask, &installed, &probe_cuda)?;
    act_on_runtime(&dir, &installed, gpu).await
}

async fn act_on_runtime(dir: &Path, installed: &Installed, gpu: Option<Flavor>) -> Result<()> {
    match runtime_action(installed, ORT_VERSION, gpu) {
        RuntimeAction::Keep => {
            if let Installed::Recorded { flavor, .. } = installed {
                println!(
                    "→ runtime: ya está ({ORT_VERSION}, {}) en {}",
                    flavor.as_str(),
                    dir.display()
                );
            }
            Ok(())
        }
        RuntimeAction::Install(flavor) => install_runtime(dir, flavor).await,
    }
}

/// What the directory holds once a swap that was cut has been finished or undone:
/// «no hay ninguno» is only true after `.previous` had its chance to come back.
fn installed_after_a_cut_swap(dir: &Path, lib_name: &str) -> Result<Installed> {
    recover_cut_swap(dir, &|from, to| std::fs::rename(from, to))?;
    Ok(installed_runtime(dir, lib_name))
}

async fn install_runtime(dir: &Path, flavor: Flavor) -> Result<()> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let (stem, ext, lib_name) = runtime_archive(os, arch, flavor)?;
    let staging = sibling(dir, "downloading");
    println!("→ runtime {} ({os}/{arch})", flavor.as_str());

    if staged_is_complete(&staging, &format!("{stem}.{ext}"), flavor, lib_name) {
        println!("  ✓ el paquete de un intento anterior está completo: no se baja de nuevo");
    } else {
        fetch_runtime(&staging, &stem, ext, lib_name, flavor).await?;
    }

    print!("  ⇄ reemplazando el runtime ... ");
    std::io::stdout().flush().ok();
    swap_runtime(
        &staging,
        dir,
        &|leaf| loads_the_pinned_runtime(leaf, lib_name),
        &|from, to| std::fs::rename(from, to),
    )?;
    println!("ok");
    println!(
        "  listo. MemoryIndustry lo encuentra en {} (o ORT_DYLIB_PATH=<ruta>)",
        dir.join(lib_name).display()
    );
    if cfg!(not(windows)) {
        println!(
            "  un daemon que ya estaba corriendo sigue con el runtime anterior hasta que lo reinicies."
        );
    }
    if flavor.is_gpu() {
        println!(
            "  nota GPU: el provider CUDA necesita las bibliotecas de {} accesibles; cuDNN es \
             opcional.\n\
             \x20      si el doctor cae a CPU, agregá sus rutas a LD_LIBRARY_PATH (Linux) o PATH (Windows).",
            cuda_libraries(Some(flavor))
        );
    }
    Ok(())
}

fn wanted_runtime_lib(base: &str, lib_name: &str, stem: &str, gpu: bool) -> bool {
    if base.starts_with(lib_name) {
        return true;
    }
    if gpu
        && base.starts_with(stem)
        && base.contains("_providers_")
        && !base.contains("tensorrt")
        && (base.contains(".so") || base.ends_with(".dll") || base.contains(".dylib"))
    {
        return true;
    }
    false
}

fn set_exec(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn extract_runtime(
    archive: &Path,
    ext: &str,
    lib_name: &str,
    dir: &Path,
    gpu: bool,
) -> Result<Vec<String>> {
    let stem = lib_name.split('.').next().unwrap_or(lib_name);
    let lib_dest = dir.join(lib_name);
    let mut extracted: Vec<String> = Vec::new();
    let mut main_real: Option<(u64, PathBuf)> = None;

    match ext {
        "tgz" => {
            let f = std::fs::File::open(archive)?;
            let gz = flate2::read::GzDecoder::new(f);
            let mut tar = tar::Archive::new(gz);
            for entry in tar.entries()? {
                let mut entry = entry?;
                if entry.header().entry_type() != tar::EntryType::Regular {
                    continue;
                }
                let base = match entry.path()?.file_name().and_then(|n| n.to_str()) {
                    Some(n) if wanted_runtime_lib(n, lib_name, stem, gpu) => n.to_string(),
                    _ => continue,
                };
                let size = entry.header().size().unwrap_or(0);
                let out_path = dir.join(&base);
                let mut out = std::fs::File::create(&out_path)?;
                std::io::copy(&mut entry, &mut out)?;
                set_exec(&out_path)?;
                if base.starts_with(lib_name) && main_real.as_ref().is_none_or(|(s, _)| size > *s) {
                    main_real = Some((size, out_path.clone()));
                }
                extracted.push(base);
            }
        }
        "zip" => {
            let f = std::fs::File::open(archive)?;
            let mut zip = zip::ZipArchive::new(f)?;
            for i in 0..zip.len() {
                let mut file = zip.by_index(i)?;
                let base = match file
                    .enclosed_name()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                {
                    Some(n) if wanted_runtime_lib(&n, lib_name, stem, gpu) => n,
                    _ => continue,
                };
                let size = file.size();
                let out_path = dir.join(&base);
                let mut out = std::fs::File::create(&out_path)?;
                std::io::copy(&mut file, &mut out)?;
                if base.starts_with(lib_name) && main_real.as_ref().is_none_or(|(s, _)| size > *s) {
                    main_real = Some((size, out_path.clone()));
                }
                extracted.push(base);
            }
        }
        other => bail!("formato de archivo desconocido: {other}"),
    }

    match main_real {
        Some((_, real)) => {
            if real != lib_dest {
                std::fs::copy(&real, &lib_dest)?;
                set_exec(&lib_dest)?;
                extracted.push(lib_name.to_string());
            }
            Ok(extracted)
        }
        None => bail!("no encontré {lib_name} dentro del runtime"),
    }
}

/// How long a download waits for a peer that goes quiet. No limit on the whole: the
/// GPU packages are up to 450 MB and a slow line is not a stalled one.
#[derive(Clone, Copy)]
struct Timeouts {
    connect: Duration,
    /// Between two reads, the wait for the headers included (reqwest's `read_timeout`).
    read: Duration,
}

impl Timeouts {
    const DOWNLOAD: Self = Self {
        connect: Duration::from_secs(30),
        read: Duration::from_secs(120),
    };
}

async fn download_to(url: &str, dest: &Path) -> Result<()> {
    download_to_with(url, dest, Timeouts::DOWNLOAD).await
}

async fn download_to_with(url: &str, dest: &Path, timeouts: Timeouts) -> Result<()> {
    let client = crate::http_client::builder()
        .user_agent(concat!("memory-industry/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(timeouts.connect)
        .read_timeout(timeouts.read)
        .build()?;
    let resp = client.get(url).send().await?.error_for_status()?;
    let expected = resp.content_length();

    let tmp = dest.with_extension("part");
    let mut file =
        std::fs::File::create(&tmp).with_context(|| format!("creando {}", tmp.display()))?;
    let mut got: u64 = 0;
    let mut stream = resp.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        got += chunk.len() as u64;
        file.write_all(&chunk)?;
    }
    file.flush()?;
    drop(file);

    if let Some(exp) = expected
        && got != exp
    {
        std::fs::remove_file(&tmp).ok();
        bail!("descarga truncada: {got} de {exp} bytes");
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// `<path>.<suffix>`, beside `path`.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

/// Fills `<leaf>.downloading` and turns it into `leaf` only once `fill` has
/// downloaded and verified everything.
///
/// The leaf used to be created before the download. `gpu::runtime_dir`,
/// `onnx::locate_onnxruntime` and `rerank.rs` pick a cache root by whether
/// that directory exists, so a download that failed left an empty
/// `onnxruntime/` they chose over a full one in the legacy root, and the
/// install degraded to the CPU or to no reranker without a word.
///
/// A failed fill keeps its staging directory, not the leaf: nothing reads it,
/// and the next run starts from the files it already verified instead of
/// downloading four gigabytes of reranker again.
async fn materialize_dir<T, F, Fut>(leaf: &Path, fill: F) -> Result<T>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let staging = sibling(leaf, "downloading");
    std::fs::create_dir_all(&staging).with_context(|| format!("creando {}", staging.display()))?;
    let filled = fill(staging.clone()).await?;
    promote(&staging, leaf)
        .with_context(|| format!("moviendo {} a {}", staging.display(), leaf.display()))?;
    Ok(filled)
}

/// A leaf that does not exist yet appears in one rename. One that does (a
/// model with a file still missing) already existed before this run, so it
/// takes the files one by one. The runtime does not come through here: its
/// directory is replaced whole (`swap_runtime`), because the files of two
/// versions side by side are not a runtime.
fn promote(staging: &Path, leaf: &Path) -> Result<()> {
    if !leaf.exists() {
        std::fs::rename(staging, leaf)?;
        return Ok(());
    }
    for entry in std::fs::read_dir(staging)? {
        let entry = entry?;
        std::fs::rename(entry.path(), leaf.join(entry.file_name()))?;
    }
    std::fs::remove_dir(staging)?;
    Ok(())
}

const IN_USE: &str = "en uso: para el daemon y vuelve a correr";

/// The files that make a cache directory usable: what the loaders look for.
/// A directory without one of them is not a model, whatever else it holds.
const RUNTIME_LIBRARIES: &[&str] = &[
    "onnxruntime.dll",
    "libonnxruntime.so",
    "libonnxruntime.dylib",
];
const MODEL_GRAPHS: &[&str] = &["model.onnx", "model_quantized.onnx"];

fn usable_artifacts(entry: &str) -> Option<&'static [&'static str]> {
    match entry {
        "onnxruntime" => Some(RUNTIME_LIBRARIES),
        "models" | "models-nli" | "models-bge-m3" | "reranker" => Some(MODEL_GRAPHS),
        _ => None,
    }
}

/// What `cache migrate` does with one entry of the legacy root.
enum Step {
    /// Only in the legacy root: it moves under the new name.
    Move(String),
    /// The same in both: the legacy copy goes.
    DropLegacy(String),
    /// Different, and the binary reads the new copy today: the legacy one is
    /// set aside as `aside` in the new root.
    KeepPreferred { name: String, aside: String },
    /// Different, and the binary reads the legacy copy today: the new one is
    /// set aside as `aside` and the legacy one takes its place.
    KeepLegacy { name: String, aside: String },
    /// `<name>.migrating`, left in the new root by a copy that was
    /// interrupted. Its source is still in the legacy root — the source is
    /// removed only after the copy is renamed into place — so the orphan is
    /// discarded and the entry redone from the source. Resuming it would mean
    /// trusting bytes nobody verified, and verifying them costs what copying
    /// them again does.
    DropOrphan(String),
}

impl Step {
    fn describe(&self) -> String {
        match self {
            Step::Move(name) => format!("{name}: solo en la caché vieja, se mueve"),
            Step::DropLegacy(name) => {
                format!("{name}: idéntico en las dos, se borra la copia vieja")
            }
            Step::KeepPreferred { name, aside } => format!(
                "{name}: distinto en las dos; queda el de la caché nueva, que es el que se lee \
                 hoy, y el viejo se aparta como {aside}"
            ),
            Step::KeepLegacy { name, aside } => format!(
                "{name}: distinto en las dos; queda el de la caché vieja, que es el que se lee \
                 hoy, y el nuevo se aparta como {aside}"
            ),
            Step::DropOrphan(name) => format!(
                "{name}.migrating: copia a medias de una corrida interrumpida, se descarta y \
                 {name} se rehace desde el origen"
            ),
        }
    }
}

/// The plan, and after `--apply` what became of each step.
pub(crate) struct Migration {
    preferred: PathBuf,
    legacy: PathBuf,
    steps: Vec<Step>,
    applied: bool,
    /// One per step once applied: `Some(reason)` for a step that was skipped.
    skipped: Vec<Option<String>>,
    legacy_removed: bool,
}

impl Migration {
    pub(crate) fn is_clean(&self) -> bool {
        self.skipped.iter().all(Option::is_none)
    }

    pub(crate) fn render(&self) -> String {
        if self.steps.is_empty() {
            return format!("nada que migrar en {}\n", self.legacy.display());
        }
        let mut out = format!("{} → {}\n", self.legacy.display(), self.preferred.display());
        if !self.applied {
            for step in &self.steps {
                out.push_str(&format!("  · {}\n", step.describe()));
            }
            out.push_str(
                "\nEsto fue un plan: no se tocó nada.\n\
                 Para aplicarlo:  memory-industry cache migrate --apply\n",
            );
            return out;
        }
        for (step, skipped) in self.steps.iter().zip(&self.skipped) {
            match skipped {
                None => out.push_str(&format!("  ✓ {}\n", step.describe())),
                Some(why) => out.push_str(&format!("  ✗ {} — {why}\n", step.describe())),
            }
        }
        if self.legacy_removed {
            out.push_str(&format!(
                "{} quedó vacía y se borró.\n",
                self.legacy.display()
            ));
        }
        out
    }
}

/// `memory-industry cache migrate`: plan (and with `apply`, carry out) the
/// move of the legacy cache root under the new name.
///
/// `stamp` names the copies set aside (`<name>.legacy-<stamp>`); it is an
/// argument so a test can pin it.
pub(crate) fn migrate_cache(roots: &CacheRoots, apply: bool, stamp: &str) -> Result<Migration> {
    let steps = plan_migration(roots, stamp)?;
    let mut migration = Migration {
        preferred: roots.preferred.clone(),
        legacy: roots.legacy.clone(),
        steps,
        applied: apply,
        skipped: Vec::new(),
        legacy_removed: false,
    };
    if !apply {
        return Ok(migration);
    }
    if !migration.steps.is_empty() {
        std::fs::create_dir_all(&roots.preferred)
            .with_context(|| format!("creando {}", roots.preferred.display()))?;
    }
    migration.skipped = migration
        .steps
        .iter()
        .map(|step| apply_step(roots, step).err().map(|e| skip_reason(&e)))
        .collect();
    // Refuses a directory that still holds anything, which is the check.
    migration.legacy_removed = std::fs::remove_dir(&roots.legacy).is_ok();
    Ok(migration)
}

fn plan_migration(roots: &CacheRoots, stamp: &str) -> Result<Vec<Step>> {
    let names = legacy_entries(&roots.legacy)?;
    // Decided on the state before anything moves: once `pgpass` has moved,
    // this would answer for a machine that no longer exists.
    let pgpass_reads_legacy =
        !roots.preferred.join("pgpass").exists() && roots.legacy.join("pgpass").exists();

    let mut steps = Vec::new();
    for name in names {
        if sibling(&roots.preferred.join(&name), "migrating").exists() {
            steps.push(Step::DropOrphan(name.clone()));
        }
        steps.push(plan_entry(roots, name, stamp, pgpass_reads_legacy)?);
    }
    Ok(steps)
}

/// The names in the legacy root, sorted so the plan reads the same twice. A
/// root that is not there is an install with nothing to migrate.
fn legacy_entries(legacy: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(legacy) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("leyendo {}", legacy.display())),
    };
    let mut names = entries
        .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}

fn plan_entry(
    roots: &CacheRoots,
    name: String,
    stamp: &str,
    pgpass_reads_legacy: bool,
) -> Result<Step> {
    let new = roots.preferred.join(&name);
    if std::fs::symlink_metadata(&new).is_err() {
        return Ok(Step::Move(name));
    }
    if same_content(&roots.legacy.join(&name), &new)? {
        return Ok(Step::DropLegacy(name));
    }
    let aside = aside_name(&roots.preferred, &name, stamp);
    Ok(if legacy_is_read(roots, &name, pgpass_reads_legacy) {
        Step::KeepLegacy { name, aside }
    } else {
        Step::KeepPreferred { name, aside }
    })
}

/// Which of two different copies the binary reads today: the rule each
/// resolver already applies, not a new one.
///
/// `pgpass_app` follows `pgpass`, because that is what every binary before
/// this one did, and the role was last altered to the password that binary
/// read. A model directory is read where its artifact is: an empty leaf left
/// by a failed download loses to a full one. Everything else resolves to the
/// new root whenever it exists there.
fn legacy_is_read(roots: &CacheRoots, name: &str, pgpass_reads_legacy: bool) -> bool {
    if name == "pgpass_app" {
        return pgpass_reads_legacy;
    }
    let Some(artifacts) = usable_artifacts(name) else {
        return false;
    };
    let usable = |dir: &Path| artifacts.iter().any(|a| has_content(&dir.join(a)));
    !usable(&roots.preferred.join(name)) && usable(&roots.legacy.join(name))
}

fn aside_name(root: &Path, name: &str, stamp: &str) -> String {
    let base = format!("{name}.legacy-{stamp}");
    let mut candidate = base.clone();
    let mut n = 1;
    while std::fs::symlink_metadata(root.join(&candidate)).is_ok() {
        n += 1;
        candidate = format!("{base}-{n}");
    }
    candidate
}

fn apply_step(roots: &CacheRoots, step: &Step) -> Result<()> {
    let old = |name: &str| roots.legacy.join(name);
    let new = |name: &str| roots.preferred.join(name);
    match step {
        Step::Move(name) => relocate(&old(name), &new(name)),
        Step::DropLegacy(name) => remove_entry(&old(name)),
        Step::KeepPreferred { name, aside } => relocate(&old(name), &new(aside)),
        Step::KeepLegacy { name, aside } => {
            std::fs::rename(new(name), new(aside))
                .with_context(|| format!("apartando {} como {aside}", new(name).display()))?;
            relocate(&old(name), &new(name))
        }
        Step::DropOrphan(name) => remove_entry(&sibling(&new(name), "migrating")),
    }
}

fn skip_reason(error: &anyhow::Error) -> String {
    if in_use(error) {
        IN_USE.to_string()
    } else {
        format!("{error:#}")
    }
}

/// A file another process holds: on Windows the runtime a daemon loaded
/// (`os error 32`, or `5` for the directory above it).
fn in_use(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io| {
            io.kind() == std::io::ErrorKind::ResourceBusy
                || (cfg!(windows) && matches!(io.raw_os_error(), Some(5 | 32 | 33)))
        })
}

/// A rename keeps the file, its mode and its ACL. Across volumes it cannot,
/// and the entry is copied instead.
fn relocate(src: &Path, dst: &Path) -> Result<()> {
    relocate_with(src, dst, &|from, to| std::fs::rename(from, to))
}

/// `relocate` with the rename handed in: on one disk `std::fs::rename` never
/// answers `CrossesDevices`, so without this the branch that turns it into a
/// copy had no test.
fn relocate_with(
    src: &Path,
    dst: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    match rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => copy_across_volumes(src, dst),
        Err(e) => Err(e).with_context(|| format!("moviendo {} a {}", src.display(), dst.display())),
    }
}

/// Copy to `<dst>.migrating`, verify size and SHA-256 of every file, rename
/// into place, and only then remove the source: a run killed at any point
/// leaves either the source whole or the destination whole, never an entry
/// half-written under its real name.
///
/// `std::fs::copy` carries the Unix mode, so `pgpass` stays 0600. On Windows
/// the copy takes the ACL of the directory it lands in, which is how
/// `setup::store_password` created it in the first place.
fn copy_across_volumes(src: &Path, dst: &Path) -> Result<()> {
    copy_across_volumes_with(src, dst, &copy_tree)
}

/// `copy_across_volumes` with the copier handed in: `copy_tree` never writes a
/// wrong byte, so without this the check that removes a bad copy had no test.
/// Only the copy is handed in; the rename of the staging copy is the real one,
/// because it is a sibling of `dst` and on its volume.
fn copy_across_volumes_with(
    src: &Path,
    dst: &Path,
    copy: &dyn Fn(&Path, &Path) -> Result<()>,
) -> Result<()> {
    let staging = sibling(dst, "migrating");
    if std::fs::symlink_metadata(&staging).is_ok() {
        remove_entry(&staging)?;
    }
    copy_verified(src, &staging, copy)?;
    std::fs::rename(&staging, dst)
        .with_context(|| format!("moviendo {} a {}", staging.display(), dst.display()))?;
    remove_entry(src)
}

/// A copy that does not match its source, file by file in size and SHA-256,
/// is removed before anything else happens.
fn copy_verified(
    src: &Path,
    copy: &Path,
    copier: &dyn Fn(&Path, &Path) -> Result<()>,
) -> Result<()> {
    copier(src, copy)
        .with_context(|| format!("copiando {} a {}", src.display(), copy.display()))?;
    if fingerprint(src, true)? != fingerprint(copy, true)? {
        remove_entry(copy)?;
        bail!(
            "la copia de {} no coincide con el origen; el origen sigue donde estaba",
            src.display()
        );
    }
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    if !std::fs::symlink_metadata(src)?.is_dir() {
        std::fs::copy(src, dst)?;
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    std::fs::read_dir(src)?.try_for_each(|entry| {
        let entry = entry?;
        copy_tree(&entry.path(), &dst.join(entry.file_name()))
    })
}

fn remove_entry(path: &Path) -> Result<()> {
    let removed = if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    removed.with_context(|| format!("borrando {}", path.display()))
}

/// Two entries hold the same thing. A file is compared by content; a
/// directory by its files and their sizes — hashing gigabytes of weights to
/// decide whether a duplicate can go costs what copying them does.
fn same_content(a: &Path, b: &Path) -> Result<bool> {
    let a_is_dir = std::fs::symlink_metadata(a)?.is_dir();
    if a_is_dir != std::fs::symlink_metadata(b)?.is_dir() {
        return Ok(false);
    }
    Ok(fingerprint(a, !a_is_dir)? == fingerprint(b, !a_is_dir)?)
}

/// Every file under `root` (or `root` itself), keyed by its path relative to
/// `root`, with its size and, when `hashed`, its SHA-256.
fn fingerprint(root: &Path, hashed: bool) -> Result<BTreeMap<PathBuf, (u64, String)>> {
    let mut seen = BTreeMap::new();
    collect_files(root, Path::new(""), hashed, &mut seen)?;
    Ok(seen)
}

fn collect_files(
    path: &Path,
    rel: &Path,
    hashed: bool,
    seen: &mut BTreeMap<PathBuf, (u64, String)>,
) -> Result<()> {
    if !std::fs::symlink_metadata(path)?.is_dir() {
        seen.insert(rel.to_path_buf(), file_stamp(path, hashed)?);
        return Ok(());
    }
    std::fs::read_dir(path)?.try_for_each(|entry| {
        let name = entry?.file_name();
        collect_files(&path.join(&name), &rel.join(&name), hashed, seen)
    })
}

fn file_stamp(path: &Path, hashed: bool) -> Result<(u64, String)> {
    let len = std::fs::metadata(path)?.len();
    let digest = if hashed {
        sha256_file(path)?
    } else {
        String::new()
    };
    Ok((len, digest))
}

/// `None` asks for the help; `Some(apply)` is a `migrate`.
fn parse_cache_args(args: &[String]) -> Result<Option<bool>> {
    let Some((first, rest)) = args.split_first() else {
        return Ok(None);
    };
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-h" | "--help" | "help"))
    {
        return Ok(None);
    }
    if first.as_str() != "migrate" {
        bail!(
            "cache: subcomando desconocido `{first}`. Usá: migrate [--apply]\n\
             (`memory-industry cache --help` para más detalle)"
        );
    }
    let mut apply = false;
    for arg in rest {
        match arg.as_str() {
            "--apply" => apply = true,
            other => bail!("cache migrate: opción desconocida `{other}` (probá --help)"),
        }
    }
    Ok(Some(apply))
}

pub fn run_cache_cli(args: &[String]) -> Result<()> {
    let Some(apply) = parse_cache_args(args)? else {
        print_cache_help();
        return Ok(());
    };
    let roots = CacheRoots::resolve()?;
    let stamp = chrono::Local::now().format("%Y%m%d").to_string();
    let migration = migrate_cache(&roots, apply, &stamp)?;
    print!("{}", migration.render());
    if !migration.is_clean() {
        bail!(
            "{} entrada(s) sin migrar. Lo demás ya está movido: correrlo de nuevo solo \
             retoma lo que falta",
            migration.skipped.iter().flatten().count()
        );
    }
    Ok(())
}

fn print_cache_help() {
    println!(
        "memory-industry cache migrate [--apply]\n\n\
         Mueve lo que quede en ~/.cache/cuba-memorys/ (la caché de antes del cambio de\n\
         nombre) a ~/.cache/memory-industry/. Sin --apply muestra el plan y no toca nada.\n\n\
           solo en la vieja         se mueve\n\
           idéntico en las dos      se borra la copia vieja\n\
           distinto en las dos      queda la copia que el binario lee hoy; la otra se\n\
                                    aparta como <nombre>.legacy-AAAAMMDD en la nueva.\n\
                                    Nunca se borra.\n\n\
         Un fichero en uso (el runtime que tiene cargado un daemon) se salta y el comando\n\
         sale con error: pará el daemon y volvé a correrlo. Lo ya movido queda movido, y\n\
         una segunda corrida sobre una caché ya migrada dice «nada que migrar»."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envs::{ScopedEnv, scratch_root};

    /// Which cache root `models …` downloads into, pinned before this home
    /// resolution moves to `envs::home()`.
    ///
    /// One of only two sites of the ten that already fail loudly rather than
    /// answering `None`. The last block asserts that the message names the two
    /// variables, not the sentence it names them in: what an operator needs is
    /// which name to define, and the wording is what a shared helper is
    /// allowed to improve.
    #[tokio::test]
    async fn the_model_cache_hangs_off_the_home_and_the_error_names_both_names() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let root = scratch_root("model-cache");
        let cache = root.join(".cache");
        let preferred = cache.join("memory-industry");
        let legacy = cache.join("cuba-memorys");
        // Never created: if USERPROFILE were read first the block below would
        // resolve under it and the assertion would name that path.
        let never_created = root.join("userprofile-only");

        {
            let _h = ScopedEnv::set("HOME", &root.display().to_string());
            let _u = ScopedEnv::set("USERPROFILE", &never_created.display().to_string());
            assert_eq!(
                cache_root().expect("HOME answers"),
                preferred,
                "HOME is read first, and with nothing downloaded yet the answer is the \
                 documented directory"
            );

            std::fs::create_dir_all(&legacy).expect("the test owns this directory");
            assert_eq!(
                cache_root().expect("HOME answers"),
                legacy,
                "a machine that downloaded before the rename keeps its models under \
                 cuba-memorys. Writing the next one under the new name leaves two \
                 half-populated caches and re-downloads a gigabyte"
            );
        }
        {
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::set("USERPROFILE", &root.display().to_string());
            assert_eq!(
                cache_root().expect("USERPROFILE answers when HOME does not"),
                legacy,
                "PowerShell and cmd.exe define only USERPROFILE, which is every Windows \
                 operator who did not start from Git Bash"
            );
        }
        {
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::cleared("USERPROFILE");
            match cache_root() {
                Ok(guessed) => panic!(
                    "resolved to {} with neither name set. A gigabyte downloaded into a \
                     guessed directory is worse than a refusal: nothing ever reads it again",
                    guessed.display()
                ),
                Err(e) => {
                    let said = format!("{e:#}");
                    assert!(
                        said.contains("HOME"),
                        "the message has to name the variable to define: {said}"
                    );
                    assert!(
                        said.contains("USERPROFILE"),
                        "naming only HOME sends a Windows operator to define the one name \
                         their shell does not use: {said}"
                    );
                }
            }
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    const STAMP: &str = "20260923";

    /// A throwaway home holding both cache roots.
    ///
    /// The migration takes its roots as arguments, so these tests leave the
    /// process environment alone. Only the one that goes through
    /// `run_cache_cli` points HOME here, under the global guard.
    struct Home {
        root: PathBuf,
        roots: CacheRoots,
    }

    impl Home {
        fn new(tag: &str) -> Self {
            let root = scratch_root(tag);
            let roots = CacheRoots::under(&root);
            Self { root, roots }
        }

        fn legacy(&self, rel: &str, body: &[u8]) -> PathBuf {
            put(&self.roots.legacy.join(rel), body)
        }

        fn preferred(&self, rel: &str, body: &[u8]) -> PathBuf {
            put(&self.roots.preferred.join(rel), body)
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn put(path: &Path, body: &[u8]) -> PathBuf {
        let parent = path
            .parent()
            .expect("every fixture file lives under a root");
        std::fs::create_dir_all(parent).expect("the test owns this directory");
        std::fs::write(path, body).expect("temp dir is writable");
        path.to_path_buf()
    }

    fn read(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// Every path under `root` with its bytes (`None` for a directory), keyed
    /// relative to `root`: what «touched nothing» and «moved byte for byte»
    /// are measured against. Directories are in it so that removing an empty
    /// one is a change too.
    fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut seen = std::collections::BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let rel = path
                    .strip_prefix(root)
                    .expect("walked from root")
                    .to_path_buf();
                if path.is_dir() {
                    seen.insert(rel, None);
                    pending.push(path);
                } else {
                    seen.insert(rel, Some(read(&path)));
                }
            }
        }
        seen
    }

    fn aside(name: &str) -> String {
        format!("{name}.legacy-{STAMP}")
    }

    #[test]
    fn a_legacy_only_cache_moves_whole_under_the_new_name() {
        let home = Home::new("migrate-legacy-only");
        home.legacy("pgpass", b"admin-secret");
        home.legacy("pgpass_app", b"app-secret");
        home.legacy("audit_key", b"hmac-key");
        home.legacy("undo/obs-1.json", b"{}");
        home.legacy("models/model_quantized.onnx", b"weights");
        home.legacy("onnxruntime/onnxruntime.dll", b"runtime");
        let before = snapshot(&home.roots.legacy);

        let done = migrate_cache(&home.roots, true, STAMP).expect("the migration runs");

        assert!(done.is_clean(), "{}", done.render());
        assert_eq!(
            snapshot(&home.roots.preferred),
            before,
            "every entry of a pre-rename install has to arrive under the new name byte for \
             byte. Whatever stays behind is what the next `models` run downloads again"
        );
        assert!(
            !home.roots.legacy.exists(),
            "an emptied legacy root is removed. Left in place, every resolver that asks \
             «does the legacy root exist» keeps answering yes"
        );
    }

    #[test]
    fn a_new_only_cache_has_nothing_to_migrate_and_is_left_alone() {
        let home = Home::new("migrate-new-only");
        home.preferred("pgpass", b"admin-secret");
        home.preferred("models/model_quantized.onnx", b"weights");
        let before = snapshot(&home.root);

        let done = migrate_cache(&home.roots, true, STAMP).expect("the migration runs");

        assert!(done.is_clean(), "{}", done.render());
        assert!(
            done.render().contains("nada que migrar"),
            "an install that never had the old name must be told so, not shown an empty \
             report it has to interpret: {}",
            done.render()
        );
        assert_eq!(
            snapshot(&home.root),
            before,
            "nothing to migrate touches nothing"
        );
    }

    /// S3: startup created the new root on its own — `pgpass_app` from
    /// `app_role_password`, `undo/` from `delete --apply` — and from then on
    /// `cache_root` judges the whole root and downloads again what the legacy
    /// one already holds.
    #[test]
    fn the_new_root_startup_created_alone_does_not_hide_the_legacy_cache() {
        let home = Home::new("migrate-s3");
        home.legacy("pgpass", b"admin-secret");
        home.legacy("pgpass_app", b"the-password-the-role-has");
        home.legacy("models/model_quantized.onnx", b"weights");
        home.legacy("undo/obs-old.json", b"old");
        home.preferred("pgpass_app", b"generated-at-startup");
        home.preferred("undo/obs-new.json", b"newer");
        let new = &home.roots.preferred;

        let done = migrate_cache(&home.roots, true, STAMP).expect("the migration runs");

        assert!(done.is_clean(), "{}", done.render());
        assert_eq!(read(&new.join("pgpass")), b"admin-secret");
        assert_eq!(
            read(&new.join("models").join("model_quantized.onnx")),
            b"weights",
            "the model that is already on disk moves; it is not downloaded a second time"
        );
        assert_eq!(
            read(&new.join("pgpass_app")),
            b"the-password-the-role-has",
            "pgpass_app follows pgpass. With the admin password only in the legacy root, the \
             binary reads the legacy pgpass_app today, and that is the password the role was \
             last altered to. Keeping the other one locks the daemon out of its own role"
        );
        assert_eq!(
            read(&new.join(aside("pgpass_app"))),
            b"generated-at-startup",
            "the copy that loses is set aside under the new root, never deleted: it is a \
             credential, and a wrong guess about which one Postgres holds must be reversible"
        );
        assert_eq!(
            read(&new.join("undo").join("obs-new.json")),
            b"newer",
            "for undo the binary reads the new root today, so that one stays in place"
        );
        assert_eq!(
            read(&new.join(aside("undo")).join("obs-old.json")),
            b"old",
            "an undo snapshot is the only copy of a deleted row"
        );
        assert!(!home.roots.legacy.exists());
        assert!(
            done.render().contains(&aside("pgpass_app")),
            "the report has to say where the other copy went: {}",
            done.render()
        );
    }

    #[test]
    fn an_entry_identical_in_both_roots_loses_only_its_legacy_copy() {
        let home = Home::new("migrate-identical");
        for rel in [
            "audit_key",
            "models/model_quantized.onnx",
            "models/tokenizer.json",
        ] {
            home.legacy(rel, rel.as_bytes());
            home.preferred(rel, rel.as_bytes());
        }
        let before = snapshot(&home.roots.preferred);

        let done = migrate_cache(&home.roots, true, STAMP).expect("the migration runs");

        assert!(done.is_clean(), "{}", done.render());
        assert_eq!(
            snapshot(&home.roots.preferred),
            before,
            "an identical copy is not a conflict: nothing is set aside and nothing in the new \
             root changes"
        );
        assert!(
            !home.roots.legacy.exists(),
            "the duplicate is a second gigabyte on disk for nothing"
        );
    }

    #[test]
    fn distinct_copies_keep_the_one_the_binary_reads_and_set_the_other_aside() {
        let home = Home::new("migrate-distinct");
        let new = &home.roots.preferred;
        home.preferred("pgpass", b"admin-new");
        home.legacy("pgpass", b"admin-old");
        home.preferred("pgpass_app", b"app-new");
        home.legacy("pgpass_app", b"app-old");
        // S5: a download that failed after `create_dir_all` left this leaf empty.
        std::fs::create_dir_all(new.join("onnxruntime")).expect("the test owns this directory");
        home.legacy("onnxruntime/onnxruntime.dll", b"runtime");
        home.preferred("reranker/model.onnx", b"the-graph-in-use");
        home.legacy("reranker/model.onnx", b"an-older-graph");

        let done = migrate_cache(&home.roots, true, STAMP).expect("the migration runs");

        assert!(done.is_clean(), "{}", done.render());
        assert_eq!(read(&new.join("pgpass")), b"admin-new");
        assert_eq!(read(&new.join(aside("pgpass"))), b"admin-old");
        assert_eq!(
            read(&new.join("pgpass_app")),
            b"app-new",
            "with a pgpass in the new root the binary reads the new pgpass_app"
        );
        assert_eq!(read(&new.join(aside("pgpass_app"))), b"app-old");
        assert_eq!(
            read(&new.join("onnxruntime").join("onnxruntime.dll")),
            b"runtime",
            "for a model directory the copy that holds the usable artifact wins. An empty leaf \
             is what `gpu::runtime_dir` picks today and then loads nothing from"
        );
        assert!(
            new.join(aside("onnxruntime")).is_dir(),
            "the empty leaf is set aside too: the rule is that nothing is deleted"
        );
        assert_eq!(
            read(&new.join("reranker").join("model.onnx")),
            b"the-graph-in-use",
            "both copies hold a model, so the one the binary reads today stays"
        );
        assert_eq!(
            read(&new.join(aside("reranker")).join("model.onnx")),
            b"an-older-graph"
        );
        assert!(!home.roots.legacy.exists());
    }

    #[test]
    fn an_aside_name_already_taken_gets_a_suffix_instead_of_overwriting() {
        let home = Home::new("migrate-aside-taken");
        let new = &home.roots.preferred;
        home.preferred("audit_key", b"key-in-use");
        home.legacy("audit_key", b"older-key");
        home.preferred(&aside("audit_key"), b"set-aside-by-an-earlier-run");

        let done = migrate_cache(&home.roots, true, STAMP).expect("the migration runs");

        assert!(done.is_clean(), "{}", done.render());
        assert_eq!(read(&new.join("audit_key")), b"key-in-use");
        assert_eq!(
            read(&new.join(aside("audit_key"))),
            b"set-aside-by-an-earlier-run",
            "a run that stopped half way and is repeated the same day must not overwrite \
             what the first one set aside"
        );
        assert_eq!(
            read(&new.join(format!("{}-2", aside("audit_key")))),
            b"older-key"
        );
    }

    #[test]
    fn without_apply_the_plan_names_every_entry_and_touches_nothing() {
        let home = Home::new("migrate-plan");
        home.legacy("pgpass", b"admin");
        home.legacy("models/model_quantized.onnx", b"weights");
        home.legacy("pgpass_app", b"app-old");
        home.preferred("pgpass_app", b"app-new");
        home.legacy("audit_key", b"same");
        home.preferred("audit_key", b"same");
        let before = snapshot(&home.root);

        let plan = migrate_cache(&home.roots, false, STAMP).expect("the plan runs");

        assert_eq!(
            snapshot(&home.root),
            before,
            "without --apply this is a plan, like `delete` without --apply"
        );
        let said = plan.render();
        for name in ["pgpass", "models", "pgpass_app", "audit_key"] {
            assert!(said.contains(name), "the plan has to name `{name}`: {said}");
        }
        assert!(
            said.contains("--apply"),
            "the plan says how to apply it: {said}"
        );
    }

    #[test]
    fn a_second_run_has_nothing_to_migrate() {
        let home = Home::new("migrate-twice");
        home.legacy("pgpass", b"admin");
        home.legacy("models/model_quantized.onnx", b"weights");
        home.preferred("pgpass_app", b"app");
        let first = migrate_cache(&home.roots, true, STAMP).expect("the first run");
        assert!(first.is_clean(), "{}", first.render());
        let after_first = snapshot(&home.root);

        let second = migrate_cache(&home.roots, true, STAMP).expect("the second run");

        assert!(second.is_clean(), "{}", second.render());
        assert!(
            second.render().contains("nada que migrar"),
            "{}",
            second.render()
        );
        assert_eq!(snapshot(&home.root), after_first, "a second run is a no-op");
    }

    /// A copy across volumes goes through `<name>.migrating`. A run killed
    /// half way leaves that directory behind with the source still intact —
    /// the source is removed only after the copy is renamed into place — so
    /// the orphan is discarded and the entry redone from the source. Resuming
    /// it would mean trusting bytes nobody verified, and verifying them costs
    /// what copying them again costs.
    #[test]
    fn a_migrating_copy_left_by_an_interrupted_run_is_discarded_and_redone() {
        let home = Home::new("migrate-orphan");
        let new = &home.roots.preferred;
        home.legacy("models/model_quantized.onnx", b"all-the-weights");
        home.preferred("models.migrating/model_quantized.onnx", b"half");
        home.preferred(
            "stray.migrating/file",
            b"nothing in legacy says what this was",
        );

        let done = migrate_cache(&home.roots, true, STAMP).expect("the migration runs");

        assert!(done.is_clean(), "{}", done.render());
        assert_eq!(
            read(&new.join("models").join("model_quantized.onnx")),
            b"all-the-weights"
        );
        assert!(
            !new.join("models.migrating").exists(),
            "the half copy is gone once the entry it belonged to is in place"
        );
        assert_eq!(
            read(&new.join("stray.migrating").join("file")),
            b"nothing in legacy says what this was",
            "an orphan whose source is not in the legacy root is not this run's to judge"
        );
        assert!(!home.roots.legacy.exists());
    }

    #[test]
    fn a_move_across_volumes_copies_verifies_and_only_then_removes_the_source() {
        let home = Home::new("migrate-copy");
        let new = &home.roots.preferred;
        home.legacy("reranker/model.onnx", b"graph");
        home.legacy("reranker/nested/weights", b"more");
        let key = home.legacy("audit_key", b"hmac-key");
        std::fs::create_dir_all(new).expect("the test owns this directory");
        let src = home.roots.legacy.join("reranker");
        let before = snapshot(&src);

        copy_across_volumes(&src, &new.join("reranker")).expect("the copy runs");
        copy_across_volumes(&key, &new.join("audit_key")).expect("the copy runs");

        assert_eq!(snapshot(&new.join("reranker")), before);
        assert_eq!(read(&new.join("audit_key")), b"hmac-key");
        assert!(!src.exists(), "the source goes once the copy is verified");
        assert!(!key.exists());
        assert!(
            !new.join("reranker.migrating").exists() && !new.join("audit_key.migrating").exists(),
            "the staging name is renamed into place, not left beside it"
        );
    }

    /// What `std::fs::rename` answers when the two roots are on different
    /// volumes. A test on one disk never gets it from the real call, which is
    /// why the branch that turns it into a copy had no test.
    fn across_volumes(_: &Path, _: &Path) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::CrossesDevices))
    }

    /// Copies, then flips one byte of the copy and keeps its length: only the
    /// SHA-256 can tell it from the source.
    fn corrupting_copy(src: &Path, dst: &Path) -> Result<()> {
        copy_tree(src, dst)?;
        let victim = dst.join("model.onnx");
        let mut bytes = read(&victim);
        bytes[0] ^= 0xFF;
        std::fs::write(&victim, bytes)?;
        Ok(())
    }

    /// Copies, then the source changes before anything compares the two: a
    /// `models` download still writing into the legacy root, say.
    fn copy_while_the_source_changes(src: &Path, dst: &Path) -> Result<()> {
        copy_tree(src, dst)?;
        std::fs::write(src.join("model.onnx"), b"rewritten after the copy")?;
        Ok(())
    }

    /// A legacy `reranker/` with two files, the new root created, and the two
    /// paths a move is handed.
    fn reranker_to_move(tag: &str) -> (Home, PathBuf, PathBuf) {
        let home = Home::new(tag);
        home.legacy("reranker/model.onnx", b"graph");
        home.legacy("reranker/nested/weights", b"more");
        std::fs::create_dir_all(&home.roots.preferred).expect("the test owns this directory");
        let src = home.roots.legacy.join("reranker");
        let dst = home.roots.preferred.join("reranker");
        (home, src, dst)
    }

    #[test]
    fn a_rename_refused_across_volumes_becomes_a_verified_copy_and_the_source_goes() {
        let (_home, src, dst) = reranker_to_move("migrate-crosses");
        let before = snapshot(&src);
        let renames = std::cell::Cell::new(0);
        let rename = |from: &Path, to: &Path| {
            renames.set(renames.get() + 1);
            across_volumes(from, to)
        };

        relocate_with(&src, &dst, &rename)
            .expect("a volume boundary is not a failure: the entry is copied instead");

        assert_eq!(
            renames.get(),
            1,
            "the rename is tried first and once. It keeps the mode and the ACL; the copy is \
             only for when the rename cannot"
        );
        assert_eq!(
            snapshot(&dst),
            before,
            "every file arrives, byte for byte, under the new name"
        );
        assert!(
            !src.exists(),
            "the source goes once the copy is verified and in place; left behind, it is a \
             second gigabyte and the next run's conflict"
        );
        assert!(
            !sibling(&dst, "migrating").exists(),
            "the staging name is renamed into place, not left beside it"
        );
    }

    #[test]
    fn a_copy_that_does_not_match_its_source_is_removed_and_the_source_stays() {
        type Copier = dyn Fn(&Path, &Path) -> Result<()>;
        let copiers: [(&str, &Copier); 2] = [
            ("migrate-corrupt-copy", &corrupting_copy),
            ("migrate-source-changes", &copy_while_the_source_changes),
        ];

        for (tag, copier) in copiers {
            let (_home, src, dst) = reranker_to_move(tag);

            let refused = copy_across_volumes_with(&src, &dst, copier)
                .expect_err("a copy whose hash differs from its source is not a move");

            let said = format!("{refused:#}");
            assert!(
                said.contains("no coincide"),
                "{tag}: the error says the copy did not match, so the operator knows nothing \
                 moved: {said}"
            );
            assert!(
                !dst.exists(),
                "{tag}: a copy that failed its check never takes the real name. Under it, the \
                 loaders would open bytes nobody verified"
            );
            assert!(
                !sibling(&dst, "migrating").exists(),
                "{tag}: the bad copy is removed, not left for the next run to trust"
            );
            assert!(
                src.join("model.onnx").exists() && src.join("nested").join("weights").exists(),
                "{tag}: the source stays where it was. It is the only copy known to be whole"
            );
        }
    }

    #[test]
    fn a_rename_that_fails_for_another_reason_is_not_turned_into_a_copy() {
        let (_home, src, dst) = reranker_to_move("migrate-rename-refused");
        let before = snapshot(&src);
        let refused = |_: &Path, _: &Path| -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        };

        let error = relocate_with(&src, &dst, &refused)
            .expect_err("a rename refused for any reason but the volume is an error");

        assert!(
            !dst.exists() && !sibling(&dst, "migrating").exists(),
            "only a volume boundary turns into a copy. A file another process holds is refused \
             by the rename too, and copying it would leave the source that cannot be removed \
             beside a second copy of it"
        );
        assert_eq!(snapshot(&src), before, "the source is untouched");
        assert!(
            error
                .chain()
                .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()),
            "the io error travels in the chain, which is where `in_use` looks for it: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn secrets_keep_their_mode_whichever_way_they_move() {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new("migrate-mode");
        let new = &home.roots.preferred;
        for name in ["pgpass", "pgpass_app", "audit_key"] {
            let path = home.legacy(name, name.as_bytes());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("the test owns this file");
        }
        home.preferred("audit_key", b"a-different-key");
        // Outside the legacy root, so the migration above does not move it first.
        let copied = put(&home.root.join("elsewhere").join("pgpass"), b"secret");
        std::fs::set_permissions(&copied, std::fs::Permissions::from_mode(0o600))
            .expect("the test owns this file");

        let done = migrate_cache(&home.roots, true, STAMP).expect("the migration runs");
        assert!(done.is_clean(), "{}", done.render());
        let across = new.join("pgpass-across");
        copy_across_volumes(&copied, &across).expect("the copy runs");

        for path in [
            new.join("pgpass"),
            new.join("pgpass_app"),
            new.join(aside("audit_key")),
            across,
        ] {
            let mode = std::fs::metadata(&path)
                .expect("moved")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode,
                0o600,
                "{} is a credential and came out {mode:o}",
                path.display()
            );
        }
    }

    /// A daemon holds `onnxruntime.dll` while it runs, and Windows refuses to
    /// move the directory under it. That entry is skipped and named, the run
    /// fails, and what could move has moved.
    #[cfg(windows)]
    #[test]
    fn a_runtime_held_open_by_a_daemon_is_skipped_named_and_fails_the_run() {
        use std::os::windows::fs::OpenOptionsExt;
        let home = Home::new("migrate-in-use");
        let new = &home.roots.preferred;
        let dll = home.legacy("onnxruntime/onnxruntime.dll", b"runtime");
        home.legacy("pgpass", b"admin");
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&dll)
            .expect("the test owns this file");

        let done = migrate_cache(&home.roots, true, STAMP)
            .expect("an entry in use is part of the verdict, not an error of the run");
        drop(held);

        assert!(
            !done.is_clean(),
            "an entry left behind has to fail the run: {}",
            done.render()
        );
        assert!(
            done.render()
                .contains("en uso: para el daemon y vuelve a correr"),
            "{}",
            done.render()
        );
        assert_eq!(
            read(&new.join("pgpass")),
            b"admin",
            "what could move, moved"
        );
        assert_eq!(
            read(&dll),
            b"runtime",
            "the runtime in use stays where it was"
        );
        assert!(
            !new.join("onnxruntime").exists() && !new.join("onnxruntime.migrating").exists(),
            "nothing half-moved is left under the new name"
        );
        assert!(
            home.roots.legacy.exists(),
            "a legacy root that still holds something is not removed"
        );
    }

    #[tokio::test]
    async fn cache_migrate_resolves_both_roots_from_the_home_and_applies_only_when_asked() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let home = Home::new("migrate-cli");
        home.legacy("pgpass", b"admin");
        let _h = ScopedEnv::set("HOME", &home.root.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &home.root.display().to_string());
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        run_cache_cli(&args(&["migrate"])).expect("the plan runs");
        assert!(
            home.roots.legacy.join("pgpass").exists(),
            "without --apply nothing moves"
        );
        assert!(
            !home.roots.preferred.exists(),
            "a plan does not even create the new root"
        );

        run_cache_cli(&args(&["migrate", "--apply"])).expect("the migration runs");
        assert_eq!(read(&home.roots.preferred.join("pgpass")), b"admin");
        assert!(!home.roots.legacy.exists());

        run_cache_cli(&args(&["migrate", "--apply"])).expect("a second run is a clean no-op");
        assert!(
            run_cache_cli(&args(&["migrar"])).is_err(),
            "an unknown subcommand is refused, not read as a plan"
        );
    }

    /// S5: `download_model` and `download_runtime` created the leaf before
    /// downloading into it, so a failed download left an empty `onnxruntime/`
    /// that `gpu::runtime_dir` and `rerank.rs` pick over a full legacy one.
    #[tokio::test]
    async fn a_download_that_fails_leaves_no_leaf_behind_and_an_existing_one_untouched() {
        let root = scratch_root("download-fails");
        std::fs::create_dir_all(&root).expect("the test owns this directory");
        let fresh = root.join("onnxruntime");

        let outcome = materialize_dir(&fresh, |staging| async move {
            std::fs::write(staging.join("onnxruntime.zip.part"), b"half an archive")
                .expect("staging is writable");
            Err::<(), _>(anyhow::anyhow!("la red se cayó a mitad de la descarga"))
        })
        .await;

        assert!(outcome.is_err(), "a failed fill is a failed download");
        assert!(
            !fresh.exists(),
            "a failed download must not leave the leaf the loaders choose by existence"
        );

        let existing = root.join("models");
        put(&existing.join("tokenizer.json"), b"tok");
        let before = snapshot(&existing);
        let outcome = materialize_dir(&existing, |staging| async move {
            std::fs::write(staging.join("model_quantized.onnx.part"), b"half")
                .expect("staging is writable");
            Err::<(), _>(anyhow::anyhow!("descarga truncada"))
        })
        .await;

        assert!(outcome.is_err());
        assert_eq!(
            snapshot(&existing),
            before,
            "a leaf that was already there is left exactly as it was"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_download_that_finishes_lands_whole_and_leaves_no_staging() {
        let root = scratch_root("download-lands");
        std::fs::create_dir_all(&root).expect("the test owns this directory");
        let fresh = root.join("reranker");
        let existing = root.join("models");
        put(&existing.join("tokenizer.json"), b"tok");

        materialize_dir(&fresh, |staging| async move {
            std::fs::write(staging.join("model.onnx"), b"graph").expect("staging is writable");
            std::fs::write(staging.join("model.onnx_data"), b"weights")
                .expect("staging is writable");
            Ok::<(), anyhow::Error>(())
        })
        .await
        .expect("the fill succeeded");
        materialize_dir(&existing, |staging| async move {
            std::fs::write(staging.join("model_quantized.onnx"), b"weights")
                .expect("staging is writable");
            Ok::<(), anyhow::Error>(())
        })
        .await
        .expect("the fill succeeded");

        assert_eq!(read(&fresh.join("model.onnx")), b"graph");
        assert_eq!(read(&fresh.join("model.onnx_data")), b"weights");
        assert_eq!(
            read(&existing.join("tokenizer.json")),
            b"tok",
            "what the leaf already held stays"
        );
        assert_eq!(read(&existing.join("model_quantized.onnx")), b"weights");
        let mut left: Vec<String> = std::fs::read_dir(&root)
            .expect("root exists")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["models", "reranker"],
            "a finished download leaves only its leaf: a staging directory beside it would \
             be one more entry for `cache migrate` and `doctor` to report"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_with_no_bytes_is_not_content_and_one_byte_is() {
        let home = Home::new("has-content");
        let empty = put(&home.root.join("empty.bin"), b"");
        let one_byte = put(&home.root.join("one-byte.bin"), b"x");

        assert!(
            !has_content(&empty),
            "an empty file is what a download that died right after creating it leaves; \
             counting it as present skips the download and the loader then fails on it"
        );
        assert!(
            has_content(&one_byte),
            "one byte is already something to verify"
        );
        assert!(
            !has_content(&home.root.join("missing.bin")),
            "a file that is not there is not content"
        );
    }

    /// The «delete the match arm» mutant on `usable_artifacts` survived because the
    /// reranker case above has a usable graph in BOTH roots, and with the arm gone
    /// that still resolves to the same step. Here the preferred leaf is the empty
    /// one a failed download leaves, so only a directory known to be a model
    /// directory can make the legacy copy win.
    #[test]
    fn every_model_directory_is_read_where_its_graph_is_not_where_it_was_made_first() {
        let home = Home::new("migrate-model-dirs");
        let new = &home.roots.preferred;
        let names = ["models", "models-nli", "models-bge-m3", "reranker"];
        for name in names {
            home.preferred(&format!("{name}/model.onnx"), b"");
            home.legacy(&format!("{name}/model.onnx"), b"graph");
        }

        let done = migrate_cache(&home.roots, true, STAMP).expect("the migration runs");

        assert!(done.is_clean(), "{}", done.render());
        for name in names {
            assert_eq!(
                read(&new.join(name).join("model.onnx")),
                b"graph",
                "{name}: the legacy copy holds the graph and the new leaf is empty, so it is \
                 the one `models` reads today and it keeps the name"
            );
            assert_eq!(
                read(&new.join(aside(name)).join("model.onnx")),
                b"",
                "{name}: the empty leaf is set aside, never deleted"
            );
        }
    }

    #[test]
    fn one_skipped_step_makes_the_run_unclean_and_the_render_names_it() {
        let skipped = Migration {
            preferred: PathBuf::from("/new"),
            legacy: PathBuf::from("/old"),
            steps: vec![Step::Move("a".into()), Step::Move("b".into())],
            applied: true,
            skipped: vec![None, Some(IN_USE.to_string())],
            legacy_removed: false,
        };
        assert!(
            !skipped.is_clean(),
            "an entry left behind has to fail the run, or `cache migrate --apply` exits 0 \
             over a runtime it could not move: {}",
            skipped.render()
        );
        assert!(skipped.render().contains("✗"), "{}", skipped.render());

        let moved = Migration {
            skipped: vec![None, None],
            ..skipped
        };
        assert!(moved.is_clean(), "{}", moved.render());
    }

    #[test]
    fn a_legacy_root_that_cannot_be_read_is_an_error_and_one_that_is_absent_is_not() {
        let home = Home::new("legacy-entries");
        let not_a_directory = put(&home.root.join("not-a-directory"), b"x");

        assert_eq!(
            legacy_entries(&home.root.join("absent"))
                .expect("an absent root has nothing to migrate"),
            Vec::<String>::new()
        );
        let error = legacy_entries(&not_a_directory)
            .expect_err("only «not found» means nothing to migrate: any other refusal is an error");
        assert!(
            format!("{error:#}").contains("leyendo"),
            "the error names what was being read: {error:#}"
        );
    }

    fn io_in_the_chain(source: std::io::Error) -> anyhow::Error {
        anyhow::Error::new(source).context("moviendo a b")
    }

    #[test]
    fn only_a_busy_file_is_in_use_and_the_windows_codes_count_only_on_windows() {
        let busy = io_in_the_chain(std::io::Error::from(std::io::ErrorKind::ResourceBusy));
        assert!(
            in_use(&busy),
            "a busy file is the one a daemon holds: {busy:#}"
        );

        let denied = io_in_the_chain(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert!(
            !in_use(&denied),
            "a refusal that is not «busy» must show its own cause, not tell the operator to \
             stop a daemon that holds nothing: {denied:#}"
        );
        assert!(
            !in_use(&anyhow::anyhow!("no io error anywhere in this chain")),
            "no io error in the chain, nothing holds anything"
        );

        for code in [5, 32, 33] {
            let held = io_in_the_chain(std::io::Error::from_raw_os_error(code));
            assert_eq!(
                in_use(&held),
                cfg!(windows),
                "os error {code} means a held file on Windows (sharing and lock violations) \
                 and something else on Linux, where the same number is EIO, EPIPE or EDOM"
            );
        }
    }

    #[test]
    fn a_skipped_entry_says_in_use_for_a_busy_file_and_gives_the_whole_cause_otherwise() {
        let busy = io_in_the_chain(std::io::Error::from(std::io::ErrorKind::ResourceBusy));
        assert_eq!(skip_reason(&busy), IN_USE);

        let other =
            anyhow::Error::new(std::io::Error::other("disk on fire")).context("apartando a como b");
        assert_eq!(
            skip_reason(&other),
            "apartando a como b: disk on fire",
            "the operator reads this line to find out why the entry stayed: the context and the \
             cause, not an empty string and not the in-use hint"
        );
    }

    /// Runs one test of this binary again as a child process and returns what it
    /// printed. `println!` in the code under test goes to the real stdout only
    /// with `--nocapture`, and a test cannot read its own: this is how a message
    /// that exists only as output is checked at all.
    fn rerun_printing(test: &str) -> String {
        let (_crate_name, in_crate) = module_path!()
            .split_once("::")
            .expect("a module path starts with the crate");
        let output = std::process::Command::new(
            std::env::current_exe().expect("the test binary knows where it lives"),
        )
        .arg(format!("{in_crate}::{test}"))
        .args(["--exact", "--nocapture"])
        .output()
        .expect("the test binary runs again");
        let printed = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success() && printed.contains("1 passed"),
            "the child run of {test} did not pass exactly one test:\n{printed}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        printed
    }

    /// The child that `the_size_of_a_staged_model_is_reported_in_whole_megabytes`
    /// reruns. It also stands on its own: a file a failed run left in staging,
    /// with no registered checksum, is adopted without touching the network.
    #[tokio::test]
    async fn a_model_file_left_in_staging_is_adopted_and_its_size_reported() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let home = Home::new("staged-model");
        let _h = ScopedEnv::set("HOME", &home.root.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &home.root.display().to_string());
        let spec = ModelSpec {
            dir: "probe-model",
            hf_repo: "probe/never-contacted",
            files: &[("remote.bin", "weights.bin")],
            env_hint: "PROBE_MODEL_PATH",
        };
        let staged = home
            .roots
            .preferred
            .join("probe-model.downloading")
            .join("weights.bin");
        put(&staged, &vec![7u8; 3 * 1_048_576 + 5]);

        download_model(&spec)
            .await
            .expect("a staged file with no registered checksum is adopted");

        assert_eq!(
            read(&home.roots.preferred.join("probe-model").join("weights.bin")).len(),
            3 * 1_048_576 + 5,
            "the staged file lands in the model directory"
        );
    }

    #[test]
    fn the_size_of_a_staged_model_is_reported_in_whole_megabytes() {
        let printed =
            rerun_printing("a_model_file_left_in_staging_is_adopted_and_its_size_reported");

        assert!(
            printed.lines().any(|line| line == "3 MB"),
            "3 MiB plus 5 bytes is «3 MB»: dividing gives 3, a remainder would print 5 and a \
             product a number with twelve digits:\n{printed}"
        );
    }

    /// The child of `asking_for_the_cache_help_prints_it`.
    #[test]
    fn the_cache_help_flag_is_a_help_not_a_migration() {
        run_cache_cli(&["--help".to_string()]).expect("asking for help is not an error");
    }

    #[test]
    fn asking_for_the_cache_help_prints_it() {
        let printed = rerun_printing("the_cache_help_flag_is_a_help_not_a_migration");

        assert!(
            printed.contains("memory-industry cache migrate [--apply]")
                && printed.contains("una segunda corrida sobre una caché ya migrada"),
            "`cache --help` that prints nothing leaves the operator with an exit 0 and no \
             idea what the command does:\n{printed}"
        );
    }

    /// An HTTPS proxy that accepts and hangs up. `download_to` builds its client
    /// from the environment, so with this in place a download that is attempted
    /// fails at once, on this machine, and the count says it was attempted.
    struct DeadProxy {
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        listener: tokio::task::JoinHandle<()>,
        _env: Vec<ScopedEnv>,
    }

    impl DeadProxy {
        async fn start() -> Self {
            let socket = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("an ephemeral loopback port is free");
            let url = format!("http://{}", socket.local_addr().expect("it is bound"));
            let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = hits.clone();
            let listener = tokio::spawn(async move {
                while let Ok((stream, _)) = socket.accept().await {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    drop(stream);
                }
            });
            let mut env: Vec<ScopedEnv> = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
                .iter()
                .map(|name| ScopedEnv::set(name, &url))
                .collect();
            env.extend(
                ["NO_PROXY", "no_proxy"]
                    .iter()
                    .map(|name| ScopedEnv::cleared(name)),
            );
            Self {
                hits,
                listener,
                _env: env,
            }
        }

        fn hits(&self) -> usize {
            self.hits.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Drop for DeadProxy {
        fn drop(&mut self) {
            self.listener.abort();
        }
    }

    /// What `models runtime` writes beside the library, by hand and not through
    /// the code under test: the format is the contract between one run and the
    /// next, so the fixtures spell it out.
    fn record_json(version: &str, flavor: &str, archive: &str) -> String {
        format!(
            r#"{{"version":"{version}","flavor":"{flavor}","archive":"{archive}","sha256":"00"}}"#
        )
    }

    /// The runtime of this platform as the host would ask for it on the CPU: its
    /// archive file name and the library it holds.
    fn host_cpu_runtime() -> (String, &'static str) {
        let (stem, ext, lib_name) =
            runtime_archive(std::env::consts::OS, std::env::consts::ARCH, Flavor::Cpu)
                .expect("a supported platform");
        (format!("{stem}.{ext}"), lib_name)
    }

    /// CI-D5b (D5b.4). Whether a runtime is downloaded is decided by what the
    /// record beside the library says, and the only way to see «no request» is a
    /// proxy that counts them. «The file exists» used to answer for every
    /// version: an install with the 1.26.0 library in place never got the
    /// 1.31.1 one. `--cuda 12` is what makes the GPU row machine-independent:
    /// a forced major answers without reading the driver, so a build host with
    /// an old driver, or none, reaches the same request.
    #[tokio::test]
    async fn the_runtime_is_downloaded_unless_the_recorded_one_is_the_pinned_one() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let home = Home::new("runtime-present");
        let _h = ScopedEnv::set("HOME", &home.root.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &home.root.display().to_string());
        let proxy = DeadProxy::start().await;
        let (archive, lib_name) = host_cpu_runtime();
        let leaf = home.roots.preferred.join("onnxruntime");
        let cpu = RuntimeArgs::default();

        let nothing_there = download_runtime(&cpu).await;
        let attempted = proxy.hits();
        assert!(
            nothing_there.is_err() && attempted >= 1,
            "no runtime in the cache means a download is attempted ({attempted} requests); \
             returning «ya está» over an empty cache leaves the install without a runtime: \
             {nothing_there:?}"
        );

        put(&leaf.join(lib_name), b"runtime");
        let unrecorded = download_runtime(&cpu).await;
        let after_unrecorded = proxy.hits();
        assert!(
            unrecorded.is_err() && after_unrecorded > attempted,
            "a library with no record beside it is a runtime of unknown version, and every \
             install made before the record existed is one: it is downloaded again \
             ({after_unrecorded} requests after {attempted}): {unrecorded:?}"
        );

        put(
            &leaf.join("runtime.json"),
            record_json(ORT_VERSION, "cpu", &archive).as_bytes(),
        );
        download_runtime(&cpu)
            .await
            .expect("the recorded runtime is the pinned one: there is nothing to do");
        assert_eq!(
            proxy.hits(),
            after_unrecorded,
            "a runtime whose record names the pinned version makes no request at all"
        );

        let gpu = download_runtime(&RuntimeArgs {
            gpu: true,
            cuda: Some(12),
            if_installed: false,
        })
        .await;
        assert!(
            gpu.is_err() && proxy.hits() > after_unrecorded,
            "`runtime --gpu` over a recorded CPU runtime exists to fetch the GPU build: the \
             pinned version of the CPU one must not answer for it: {gpu:?}"
        );
    }

    /// The Windows runtime ships as a zip, and `extract_runtime` reads it with
    /// `by_index` + `enclosed_name`, never with `ZipArchive::extract`, which is
    /// the call RUSTSEC-2026-0367, -0368 and -0369 are about. No test opened a
    /// zip before `zip` moved from 2 to 9 (a new deflate backend, among other
    /// things), so this pins what the branch does with one: it keeps the library
    /// `lib_name` names wherever it sits in the archive, leaves the rest on the
    /// floor, and writes nothing for an entry whose name climbs out of the
    /// directory. That entry comes last and says something else, so if its name
    /// were ever trusted it would overwrite the library and the bytes would show it.
    #[test]
    fn the_zip_runtime_keeps_its_library_and_nothing_that_climbs_out_of_the_directory() {
        use std::io::Write;

        let root = scratch_root("zip-runtime");
        let dir = root.join("onnxruntime");
        std::fs::create_dir_all(&dir).expect("the test owns this directory");
        let archive = root.join("onnxruntime.zip");

        let library: &[u8] = b"the library the daemon loads, long enough to be the largest";
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut writer =
            zip::ZipWriter::new(std::fs::File::create(&archive).expect("the test owns this file"));
        for (name, bytes) in [
            ("onnxruntime-win-x64-1.2.3/lib/onnxruntime.dll", library),
            ("onnxruntime-win-x64-1.2.3/lib/onnxruntime.lib", b"import"),
            ("onnxruntime-win-x64-1.2.3/README.md", b"readme"),
            (
                "onnxruntime-win-x64-1.2.3/lib/onnxruntime_providers_cuda.dll",
                b"cuda",
            ),
            ("../escaped/onnxruntime.dll", b"climbing"),
        ] {
            writer
                .start_file(name, options)
                .expect("a name the writer takes");
            writer.write_all(bytes).expect("the test owns this file");
        }
        writer.finish().expect("the archive is closed");

        let extracted = extract_runtime(&archive, "zip", "onnxruntime.dll", &dir, false)
            .expect("a zip with the library in it extracts");

        assert_eq!(
            extracted,
            vec!["onnxruntime.dll".to_string()],
            "only the library named by lib_name is wanted on a CPU install"
        );
        assert_eq!(
            std::fs::read(dir.join("onnxruntime.dll")).expect("the library was written"),
            library,
            "the library is the entry inside the runtime, not the one that climbed out"
        );
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .expect("the directory exists")
            .map(|e| {
                e.expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["onnxruntime.dll"],
            "nothing else is left in the directory"
        );
        assert!(
            !root.join("escaped").exists(),
            "an entry that climbs out of the directory writes nothing outside it"
        );
        std::fs::remove_dir_all(&root).expect("the test owns this directory");
    }

    /// CI-D2: `sha256_file` decides whether a downloaded model is kept or
    /// deleted as a «checksum mismatch», and `cache migrate` fingerprints files
    /// with it. The second file is a million bytes, far past the 8 KiB buffer of
    /// `io::copy`, so the sum has to survive more than one read: a writer that
    /// drops or repeats part of a chunk still gets «abc» right.
    #[test]
    fn the_file_checksum_is_the_fips_vector_whatever_the_reads() {
        let root = scratch_root("sha256-file");
        std::fs::create_dir_all(&root).expect("the test owns this directory");
        let short = root.join("abc.bin");
        let long = root.join("million-a.bin");
        std::fs::write(&short, b"abc").expect("temp dir is writable");
        std::fs::write(&long, vec![b'a'; 1_000_000]).expect("temp dir is writable");

        // FIPS 180-2, appendix B.1; `printf abc | sha256sum`.
        assert_eq!(
            sha256_file(&short).expect("the file is readable"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "the digest of a file that fits in one read"
        );
        // FIPS 180-2, appendix B.3; `head -c 1000000 /dev/zero | tr '\0' a | sha256sum`.
        assert_eq!(
            sha256_file(&long).expect("the file is readable"),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
            "a file larger than one read has to be hashed whole: a short sum deletes a good \
             model and a long one keeps a truncated download"
        );

        std::fs::remove_dir_all(&root).expect("the test owns this directory");
    }

    /// CI-D5: `memory-industry models runtime` downloads `ORT_VERSION`, and the
    /// binary refuses any runtime whose minor number is below
    /// `ort::MINOR_VERSION`, the one the `api-24` feature of `ort` sets. The two
    /// live in different files and nothing tied them: raising `api-NN` above the
    /// download would leave every new install with a runtime its own binary
    /// rejects. The minor is read the way `ort` reads it (the number after the
    /// first dot, 0 when there is none), and the floor is `ort`'s, not a literal
    /// repeated here.
    #[test]
    fn the_runtime_the_installer_fetches_meets_the_floor_of_ort() {
        let fetched_minor = ORT_VERSION
            .split('.')
            .nth(1)
            .and_then(|minor| minor.parse::<u32>().ok())
            .unwrap_or(0);
        assert!(
            fetched_minor >= ort::MINOR_VERSION,
            "`models runtime` downloads ONNX Runtime {ORT_VERSION} (minor {fetched_minor}) and \
             this build of `ort` loads only 1.{}.x or newer: every new install would get a \
             runtime its own binary refuses. Raise ORT_VERSION, or lower the `api-NN` feature \
             of `ort` in Cargo.toml",
            ort::MINOR_VERSION
        );
    }

    /// CI-D5b (D5b.1). The names of the packages are compared with the ones
    /// the release `v1.31.1` of microsoft/onnxruntime publishes, copied from its
    /// asset list, and not with a `format!` of the same pattern the code
    /// uses: a pattern that is wrong would be wrong in both. `-gpu-` is the
    /// name of 1.26.0 and answers 404 from 1.27.0 on, which is what the
    /// `cuda12` row of this table guards.
    #[test]
    fn the_installer_asks_for_the_packages_of_the_release_by_platform_and_flavor() {
        let rows: [(&str, &str, Flavor, &str, &str); 8] = [
            (
                "linux",
                "x86_64",
                Flavor::Cpu,
                "onnxruntime-linux-x64-1.31.1.tgz",
                "libonnxruntime.so",
            ),
            (
                "linux",
                "x86_64",
                Flavor::Cuda12,
                "onnxruntime-linux-x64-gpu_cuda12-1.31.1.tgz",
                "libonnxruntime.so",
            ),
            (
                "linux",
                "x86_64",
                Flavor::Cuda13,
                "onnxruntime-linux-x64-gpu_cuda13-1.31.1.tgz",
                "libonnxruntime.so",
            ),
            (
                "linux",
                "aarch64",
                Flavor::Cpu,
                "onnxruntime-linux-aarch64-1.31.1.tgz",
                "libonnxruntime.so",
            ),
            (
                "macos",
                "aarch64",
                Flavor::Cpu,
                "onnxruntime-osx-arm64-1.31.1.tgz",
                "libonnxruntime.dylib",
            ),
            (
                "windows",
                "x86_64",
                Flavor::Cpu,
                "onnxruntime-win-x64-1.31.1.zip",
                "onnxruntime.dll",
            ),
            (
                "windows",
                "x86_64",
                Flavor::Cuda12,
                "onnxruntime-win-x64-gpu_cuda12-1.31.1.zip",
                "onnxruntime.dll",
            ),
            (
                "windows",
                "x86_64",
                Flavor::Cuda13,
                "onnxruntime-win-x64-gpu_cuda13-1.31.1.zip",
                "onnxruntime.dll",
            ),
        ];
        assert_eq!(
            ORT_VERSION, "1.31.1",
            "the names above are the ones of the release the installer is pinned to"
        );
        for (os, arch, flavor, file, library) in rows {
            let row = format!("{os}/{arch} {flavor:?}");
            let (stem, ext, lib_name) = runtime_archive(os, arch, flavor)
                .unwrap_or_else(|e| panic!("{row}: the release has this package: {e:#}"));
            assert_eq!(
                format!("{stem}.{ext}"),
                file,
                "{row}: the file name of the release"
            );
            assert_eq!(lib_name, library, "{row}: the library the loader looks for");
        }

        for flavor in [Flavor::Cpu, Flavor::Cuda13] {
            let said = format!(
                "{:#}",
                runtime_archive("macos", "x86_64", flavor)
                    .expect_err("there is no macOS x86_64 package since 1.24")
            );
            assert!(
                said.contains("1.24") && said.contains("ORT_DYLIB_PATH"),
                "macos/x86_64 {flavor:?}: the error says since when there is no package and what to \
                 do instead, and not a 404 later: {said}"
            );
        }
        for (os, arch) in [("linux", "aarch64"), ("macos", "aarch64")] {
            for flavor in [Flavor::Cuda12, Flavor::Cuda13] {
                let said = format!(
                    "{:#}",
                    runtime_archive(os, arch, flavor)
                        .expect_err("Microsoft publishes no GPU core package for this platform")
                );
                assert!(
                    said.contains("sin --gpu baja el de CPU"),
                    "{os}/{arch} {flavor:?}: asking for a GPU where there is none must say so, not \
                     hand over the CPU one in silence: {said}"
                );
            }
        }
        assert!(
            runtime_archive("windows", "aarch64", Flavor::Cpu).is_err(),
            "no package is promised for a platform the table does not know"
        );
    }

    /// The eight packages of `the_installer_asks_for_the_packages_…` with the
    /// first twelve characters of the digest GitHub shows for each asset of
    /// the release. A pinned sum that belongs to another package of the table
    /// is as wrong as a missing one, and a length check does not see it.
    const PINNED_PACKAGES: [(&str, &str, Flavor, &str); 8] = [
        ("linux", "x86_64", Flavor::Cpu, "c849e36a1ef3"),
        ("linux", "x86_64", Flavor::Cuda12, "57cd51f677ec"),
        ("linux", "x86_64", Flavor::Cuda13, "b2146862e691"),
        ("linux", "aarch64", Flavor::Cpu, "5317505e2da9"),
        ("macos", "aarch64", Flavor::Cpu, "2e1344a1926e"),
        ("windows", "x86_64", Flavor::Cpu, "5869e26bb253"),
        ("windows", "x86_64", Flavor::Cuda12, "5cbf65edc93d"),
        ("windows", "x86_64", Flavor::Cuda13, "6c64508de9c0"),
    ];

    /// CI-D5b (D5b.2). The runtime used to be extracted without a checksum
    /// while the models had one. Each package the installer can ask for has its
    /// sum pinned under `onnxruntime/<file>`.
    #[test]
    fn every_package_the_installer_can_ask_for_has_its_pinned_checksum() {
        for (os, arch, flavor, prefix) in PINNED_PACKAGES {
            let (stem, ext, _) = runtime_archive(os, arch, flavor).expect("a package of the table");
            let key = format!("onnxruntime/{stem}.{ext}");
            let sum = expected_digest(&key).unwrap_or_else(|| {
                panic!("{key} has no pinned checksum: the installer would extract it unchecked")
            });
            assert_eq!(sum.len(), 64, "{key}: a SHA-256 in hex");
            assert!(
                sum.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "{key}: lowercase hex, as `sha256_file` writes it"
            );
            assert!(
                sum.starts_with(prefix),
                "{key}: the pinned sum starts {} and the release says {prefix}: it is the sum of \
                 another package, or of another version",
                &sum[..12]
            );
        }
    }

    /// A tarball with one regular file, which is the shape of the CPU package
    /// of Linux for what `extract_runtime` reads.
    fn tiny_runtime_tgz(path: &Path, library: &[u8]) {
        let file = std::fs::File::create(path).expect("the test owns this file");
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(library.len() as u64);
        header.set_mode(0o755);
        builder
            .append_data(
                &mut header,
                "onnxruntime-linux-x64-1.2.3/lib/libonnxruntime.so.1.2.3",
                library,
            )
            .expect("a name the writer takes");
        builder
            .into_inner()
            .expect("the tar is closed")
            .finish()
            .expect("the gzip is closed");
    }

    /// CI-D5b (D5b.2). The checksum is judged before anything is extracted: a
    /// package that does not match is deleted and leaves nothing behind, and a
    /// package with no pinned sum is refused as well, because «no checksum
    /// registered» is a line for a model and a hole for a library that is loaded
    /// into the process. The table is handed in, so no real package is needed.
    #[test]
    fn a_package_that_does_not_match_its_checksum_is_deleted_and_nothing_is_extracted() {
        let home = Home::new("runtime-checksum");
        let leaf = home.root.join("onnxruntime");
        put(&leaf.join("libonnxruntime.so"), b"the runtime in use");
        let leaf_before = snapshot(&leaf);
        let staging = sibling(&leaf, "downloading");
        std::fs::create_dir_all(&staging).expect("the test owns this directory");
        let archive = staging.join("pkg.tgz");
        let key = "onnxruntime/pkg.tgz";

        tiny_runtime_tgz(
            &archive,
            b"a library, long enough to be the one that is kept",
        );
        let right = sha256_file(&archive).expect("the archive is readable");
        let wrong = "0".repeat(64);

        let kept = unpack_runtime(
            &[(key, right.as_str())],
            &archive,
            key,
            "tgz",
            "libonnxruntime.so",
            &staging,
            false,
        )
        .expect(
            "an archive that matches its sum is extracted: this is the control of the rows below",
        );
        assert!(
            kept.iter().any(|name| name == "libonnxruntime.so"),
            "the library the loader looks for is among what was extracted: {kept:?}"
        );
        assert!(staging.join("libonnxruntime.so").is_file());
        for name in &kept {
            std::fs::remove_file(staging.join(name)).expect("the test owns this file");
        }

        let altered = unpack_runtime(
            &[(key, wrong.as_str())],
            &archive,
            key,
            "tgz",
            "libonnxruntime.so",
            &staging,
            false,
        )
        .expect_err("a package that does not match its sum must not be extracted");
        let said = format!("{altered:#}");
        assert!(
            said.contains("checksum mismatch") && said.contains(key),
            "the error names the package and the mismatch: {said}"
        );
        assert!(
            !archive.exists(),
            "the package that did not match is deleted, as a model is"
        );
        assert!(
            !staging.join("libonnxruntime.so").exists(),
            "nothing is extracted from a package that did not match"
        );
        assert_eq!(
            snapshot(&leaf),
            leaf_before,
            "the runtime in use stays as it was"
        );

        tiny_runtime_tgz(
            &archive,
            b"a library, long enough to be the one that is kept",
        );
        let unpinned = unpack_runtime(
            &[("onnxruntime/another.tgz", right.as_str())],
            &archive,
            key,
            "tgz",
            "libonnxruntime.so",
            &staging,
            false,
        )
        .expect_err("a package with no pinned sum must not be extracted");
        assert!(
            format!("{unpinned:#}").contains(key),
            "the error names the package that has no sum: {unpinned:#}"
        );
        assert!(
            !staging.join("libonnxruntime.so").exists(),
            "nothing is extracted from a package that has no sum"
        );
    }

    /// CI-D5b (D5b.3) and CI-D5c (A.5). One row per line of the table of the spec,
    /// named in the message so a red says which machine it is. A driver whose card
    /// does not answer (`compute: None`) still has a version, and the version
    /// alone decides every row where the card would not have.
    #[test]
    fn cuda_comes_from_the_driver_and_the_card_and_the_flag_forces_it() {
        let card = |driver: i32, compute: Option<(i32, i32)>| Some(CudaProbe { driver, compute });
        let says = |driver: i32, compute: Option<(i32, i32)>, forced: Option<u8>| {
            choose_cuda(forced, &|| card(driver, compute))
        };

        let (flavor, note) = choose_cuda(None, &|| None).expect("no driver is not an error");
        assert_eq!(flavor, Flavor::Cuda13, "no driver: CUDA 13");
        let note = note.expect("no driver says so");
        assert!(
            note.contains("no veo driver NVIDIA") && note.contains("--cuda 12"),
            "the line that says what was not seen and how to ask for 12: {note}"
        );

        for (row, driver, compute, want) in [
            ("driver 13.3, SM 8.9", 13030, Some((8, 9)), Flavor::Cuda13),
            (
                "driver 13.3, SM 7.5 (Turing)",
                13030,
                Some((7, 5)),
                Flavor::Cuda13,
            ),
            (
                "driver 13.3, SM 6.1 (Pascal)",
                13030,
                Some((6, 1)),
                Flavor::Cuda12,
            ),
            ("driver 12.8, SM 8.9", 12080, Some((8, 9)), Flavor::Cuda12),
            ("driver 12.0, SM 6.1", 12000, Some((6, 1)), Flavor::Cuda12),
            ("driver 12.8, card not read", 12080, None, Flavor::Cuda12),
            ("driver 13.3, card not read", 13030, None, Flavor::Cuda13),
        ] {
            let (got, _) = says(driver, compute, None)
                .unwrap_or_else(|e| panic!("{row}: this machine can take a runtime: {e:#}"));
            assert_eq!(got, want, "{row}");
        }
        let (_, pascal) = says(13030, Some((6, 1)), None).expect("CUDA 12 takes it");
        assert!(
            pascal.is_some_and(|line| line.contains("6.1")),
            "falling to CUDA 12 because of the card says which capability it read"
        );
        let (_, healthy) = says(13030, Some((8, 9)), None).expect("CUDA 13 takes it");
        assert_eq!(
            healthy, None,
            "a machine that needs no explanation gets none"
        );
        let (_, unread) = says(13030, None, None).expect("CUDA 13 takes a driver of 13.3");
        assert!(
            unread.is_some_and(|line| line.contains("tarjeta 0") && line.contains("--cuda 12")),
            "a card that did not answer under a driver of 13 says it went to CUDA 13 without \
             reading it, and how to ask for 12 if the card is older than Turing"
        );
        let (_, driver_12) = says(12080, None, None).expect("CUDA 12 takes a driver of 12.8");
        assert_eq!(
            driver_12, None,
            "the version of the driver decides below 13: nothing to explain about a card that \
             was never needed"
        );

        let (forced, _) = says(13030, Some((8, 9)), Some(12)).expect("12 is forced");
        assert_eq!(
            forced,
            Flavor::Cuda12,
            "--cuda 12 wins over a driver of 13.3"
        );
        let (forced, _) = says(12080, Some((8, 9)), Some(13)).expect("13 is forced");
        assert_eq!(
            forced,
            Flavor::Cuda13,
            "--cuda 13 wins over a driver of 12.8"
        );
        let (forced, _) = says(13030, None, Some(13)).expect("13 is forced");
        assert_eq!(
            forced,
            Flavor::Cuda13,
            "--cuda 13 over a card that did not answer: there is no capability to refuse it with"
        );

        let old = format!(
            "{:#}",
            says(11080, Some((8, 9)), None)
                .expect_err("a driver older than CUDA 12 cannot run either package")
        );
        assert!(
            old.contains("11.8") && old.contains("12 o 13"),
            "the error says the version the driver supports and what the runtime needs: {old}"
        );
        let old_unread = format!(
            "{:#}",
            says(11080, None, None).expect_err("the driver version is not lost with the card")
        );
        assert!(
            old_unread.contains("11.8") && old_unread.contains("12 o 13"),
            "a driver older than CUDA 12 whose card did not answer is the same error, not CUDA \
             13 for a driver that cannot run it: {old_unread}"
        );
        let pascal = format!(
            "{:#}",
            says(13030, Some((6, 1)), Some(13)).expect_err("forcing CUDA 13 does not fix the card")
        );
        assert!(
            pascal.contains("6.1") && pascal.contains("--cuda 12"),
            "the error says the capability it read and the way out: {pascal}"
        );
    }

    /// CI-D5b (D5b.4). The table of §4.2 of the spec, one row per case. `gpu` is
    /// the flavor `--gpu` resolved to (the caller resolves it for an install
    /// without a record that holds a CUDA provider too, which is the last row
    /// but one).
    #[test]
    fn a_runtime_is_replaced_only_when_its_record_does_not_name_the_version_and_flavor_asked() {
        let recorded = |version: &str, flavor: Flavor| Installed::Recorded {
            version: version.to_string(),
            flavor,
        };
        let pinned = "1.31.1";
        for (row, installed, gpu, want) in [
            (
                "nothing, CPU",
                Installed::Nothing,
                None,
                RuntimeAction::Install(Flavor::Cpu),
            ),
            (
                "nothing, --gpu (13)",
                Installed::Nothing,
                Some(Flavor::Cuda13),
                RuntimeAction::Install(Flavor::Cuda13),
            ),
            (
                "pinned CPU, CPU",
                recorded(pinned, Flavor::Cpu),
                None,
                RuntimeAction::Keep,
            ),
            (
                "pinned CUDA 13, no --gpu",
                recorded(pinned, Flavor::Cuda13),
                None,
                RuntimeAction::Keep,
            ),
            (
                "pinned CUDA 13, --gpu 13",
                recorded(pinned, Flavor::Cuda13),
                Some(Flavor::Cuda13),
                RuntimeAction::Keep,
            ),
            (
                "pinned CPU, --gpu 13",
                recorded(pinned, Flavor::Cpu),
                Some(Flavor::Cuda13),
                RuntimeAction::Install(Flavor::Cuda13),
            ),
            (
                "pinned CUDA 13, --gpu --cuda 12",
                recorded(pinned, Flavor::Cuda13),
                Some(Flavor::Cuda12),
                RuntimeAction::Install(Flavor::Cuda12),
            ),
            (
                "pinned CUDA 12, --gpu 13",
                recorded(pinned, Flavor::Cuda12),
                Some(Flavor::Cuda13),
                RuntimeAction::Install(Flavor::Cuda13),
            ),
            (
                "1.26.0 CPU, no --gpu: the same flavor",
                recorded("1.26.0", Flavor::Cpu),
                None,
                RuntimeAction::Install(Flavor::Cpu),
            ),
            (
                "1.26.0 CUDA 12, no --gpu: the same flavor",
                recorded("1.26.0", Flavor::Cuda12),
                None,
                RuntimeAction::Install(Flavor::Cuda12),
            ),
            (
                "1.26.0 CUDA 12, --gpu 13",
                recorded("1.26.0", Flavor::Cuda12),
                Some(Flavor::Cuda13),
                RuntimeAction::Install(Flavor::Cuda13),
            ),
            (
                "no record, no providers",
                Installed::Unrecorded {
                    gpu_provider: false,
                },
                None,
                RuntimeAction::Install(Flavor::Cpu),
            ),
            (
                "no record, a CUDA provider beside it (resolved to 13)",
                Installed::Unrecorded { gpu_provider: true },
                Some(Flavor::Cuda13),
                RuntimeAction::Install(Flavor::Cuda13),
            ),
            (
                "no record, no providers, --gpu 12",
                Installed::Unrecorded {
                    gpu_provider: false,
                },
                Some(Flavor::Cuda12),
                RuntimeAction::Install(Flavor::Cuda12),
            ),
        ] {
            assert_eq!(runtime_action(&installed, pinned, gpu), want, "row «{row}»");
        }
    }

    /// CI-D5b (D5b.4). What the directory says about the runtime in it: the
    /// record when there is one that reads, and otherwise the library and, to
    /// tell the flavor, whether a CUDA provider sits beside it.
    #[test]
    fn the_runtime_in_a_directory_is_its_record_or_what_can_be_deduced_from_its_files() {
        let lib = "libonnxruntime.so";
        let dir_of = |tag: &str| {
            let home = Home::new(tag);
            let dir = home.root.join("onnxruntime");
            (home, dir)
        };

        let (_empty, dir) = dir_of("installed-empty");
        std::fs::create_dir_all(&dir).expect("the test owns this directory");
        assert!(
            matches!(installed_runtime(&dir, lib), Installed::Nothing),
            "an empty directory is no runtime"
        );
        assert!(
            matches!(
                installed_runtime(&dir.join("not-there"), lib),
                Installed::Nothing
            ),
            "a directory that does not exist is no runtime"
        );

        let (_lib_only, dir) = dir_of("installed-lib");
        put(&dir.join(lib), b"core");
        assert!(
            matches!(
                installed_runtime(&dir, lib),
                Installed::Unrecorded {
                    gpu_provider: false
                }
            ),
            "a library with no record and no provider is a CPU runtime of unknown version"
        );

        let (_gpu, dir) = dir_of("installed-gpu");
        put(&dir.join(lib), b"core");
        put(&dir.join("libonnxruntime_providers_cuda.so"), b"provider");
        assert!(
            matches!(
                installed_runtime(&dir, lib),
                Installed::Unrecorded { gpu_provider: true }
            ),
            "a CUDA provider beside the library makes it a GPU runtime of unknown version"
        );

        let (_recorded, dir) = dir_of("installed-recorded");
        put(&dir.join(lib), b"core");
        put(
            &dir.join("runtime.json"),
            record_json(
                "1.31.1",
                "cuda13",
                "onnxruntime-linux-x64-gpu_cuda13-1.31.1.tgz",
            )
            .as_bytes(),
        );
        match installed_runtime(&dir, lib) {
            Installed::Recorded { version, flavor } => {
                assert_eq!(version, "1.31.1");
                assert_eq!(flavor, Flavor::Cuda13);
            }
            other => panic!(
                "a readable record is what the directory says: {}",
                describe(&other)
            ),
        }

        let (_garbled, dir) = dir_of("installed-garbled");
        put(&dir.join(lib), b"core");
        put(&dir.join("runtime.json"), b"{ this is not json");
        assert!(
            matches!(
                installed_runtime(&dir, lib),
                Installed::Unrecorded {
                    gpu_provider: false
                }
            ),
            "a record that does not read is no record: it must not stop a reinstall"
        );

        let (_orphan, dir) = dir_of("installed-orphan-record");
        put(
            &dir.join("runtime.json"),
            record_json("1.31.1", "cpu", "x").as_bytes(),
        );
        assert!(
            matches!(installed_runtime(&dir, lib), Installed::Nothing),
            "a record with no library beside it is not a runtime"
        );
    }

    fn describe(installed: &Installed) -> &'static str {
        match installed {
            Installed::Nothing => "Nothing",
            Installed::Recorded { .. } => "Recorded",
            Installed::Unrecorded { .. } => "Unrecorded",
        }
    }

    /// The files of a runtime directory of the old version and of the new one.
    /// The old one has a provider the new one does not: that file is what a
    /// swap file by file leaves behind, next to a core that is not its own.
    fn old_and_new_runtime(home: &Home) -> (PathBuf, PathBuf) {
        let leaf = home.root.join("onnxruntime");
        let staging = sibling(&leaf, "downloading");
        put(&leaf.join("libonnxruntime.so"), b"old core 1.26.0");
        put(
            &leaf.join("libonnxruntime_providers_cuda.so"),
            b"old provider",
        );
        put(&staging.join("libonnxruntime.so"), b"new core 1.31.1");
        put(
            &staging.join("runtime.json"),
            record_json("1.31.1", "cpu", "x").as_bytes(),
        );
        (leaf, staging)
    }

    fn really_rename(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }

    /// CI-D5b (D5b.5). The new runtime takes the place of the old in one step,
    /// and the old one comes back if the new does not load. «In one step» is
    /// what the files say afterwards: a core of 1.31.1 beside a provider of
    /// 1.26.0 is a runtime nobody built and nothing refuses.
    #[test]
    fn the_new_runtime_replaces_the_old_in_one_step_and_the_old_comes_back_if_the_new_does_not_load()
     {
        let home = Home::new("swap-accepts");
        let (leaf, staging) = old_and_new_runtime(&home);
        let new = snapshot(&staging);
        swap_runtime(&staging, &leaf, &|_| Ok(()), &really_rename)
            .expect("a runtime that loads replaces the old one");
        assert_eq!(
            snapshot(&leaf),
            new,
            "the directory has the files of the new runtime and no other: a provider of the old \
             one beside the new core is the mix this swap exists to prevent"
        );
        assert!(
            !sibling(&leaf, "previous").exists(),
            "the old runtime is deleted once the new one has loaded"
        );
        assert!(
            !staging.exists(),
            "the staging directory became the runtime"
        );

        let home = Home::new("swap-refuses");
        let (leaf, staging) = old_and_new_runtime(&home);
        let old = snapshot(&leaf);
        let new = snapshot(&staging);
        let error = swap_runtime(
            &staging,
            &leaf,
            &|_| Err(anyhow::anyhow!("the library says it is version 1.26.0")),
            &really_rename,
        )
        .expect_err("a runtime that does not load does not replace the old one");
        assert!(
            format!("{error:#}").contains("the library says it is version 1.26.0"),
            "the error carries the cause the check gave: {error:#}"
        );
        assert_eq!(
            snapshot(&leaf),
            old,
            "the old runtime is back, file for file: a machine must never be left without one"
        );
        assert!(
            !sibling(&leaf, "previous").exists(),
            "and the aside copy became the runtime again"
        );
        assert_eq!(
            snapshot(&sibling(&leaf, "failed")),
            new,
            "the one that did not load is kept to be looked at, not deleted"
        );
    }

    /// CI-D5b (D5b.6). On Windows a directory with a DLL loaded does not rename.
    /// The runtime in use is not touched and the package already downloaded
    /// stays, so the next run does not download it again.
    #[test]
    fn a_runtime_in_use_is_not_touched_and_its_download_stays_for_the_next_run() {
        let home = Home::new("swap-in-use");
        let (leaf, staging) = old_and_new_runtime(&home);
        let old = snapshot(&leaf);
        let new = snapshot(&staging);
        let held = leaf.clone();
        let error = swap_runtime(
            &staging,
            &leaf,
            &|_| panic!("nothing was swapped, so there is nothing to load"),
            &move |from, to| {
                if from == held {
                    Err(std::io::Error::from(std::io::ErrorKind::ResourceBusy))
                } else {
                    std::fs::rename(from, to)
                }
            },
        )
        .expect_err("a runtime another process holds is not replaced");
        assert!(
            format!("{error:#}").contains("en uso: para el daemon y vuelve a correr"),
            "the operator is told what to do, in the words `cache migrate` already uses: {error:#}"
        );
        assert_eq!(snapshot(&leaf), old, "the runtime in use is intact");
        assert_eq!(
            snapshot(&staging),
            new,
            "the download is complete and stays: 300 MB are not fetched twice for a daemon that \
             was running"
        );
        assert!(!sibling(&leaf, "previous").exists());
    }

    /// CI-D5b (D5b.6, the next run). A complete staging directory left by a swap
    /// that found the runtime in use is the download of the next run: it makes
    /// no request. Its library here is not one, so the swap puts the old runtime
    /// back; what this pins is the request count and that nothing is lost.
    #[tokio::test]
    async fn a_complete_staging_directory_is_used_and_nothing_is_downloaded_again() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let home = Home::new("runtime-staged");
        let _h = ScopedEnv::set("HOME", &home.root.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &home.root.display().to_string());
        let proxy = DeadProxy::start().await;
        let (archive, lib_name) = host_cpu_runtime();
        let leaf = home.roots.preferred.join("onnxruntime");
        put(&leaf.join(lib_name), b"old core");
        put(
            &leaf.join("runtime.json"),
            record_json("1.26.0", "cpu", &archive).as_bytes(),
        );
        let staging = sibling(&leaf, "downloading");
        put(&staging.join(lib_name), b"not a library");
        put(
            &staging.join("runtime.json"),
            record_json(ORT_VERSION, "cpu", &archive).as_bytes(),
        );
        let old = snapshot(&leaf);

        let result = download_runtime(&RuntimeArgs::default()).await;

        assert_eq!(
            proxy.hits(),
            0,
            "the staging directory holds the package of this version, already checked and \
             extracted: nothing is requested"
        );
        assert!(
            result.is_err(),
            "the library of the fixture is not a library, so the swap is refused: {result:?}"
        );
        assert_eq!(
            snapshot(&leaf),
            old,
            "and the runtime that was there is back as it was"
        );
    }

    /// CI-D5b, P1-B. `npm install` runs `models runtime --if-installed` after it
    /// fetches the binary: a machine that never had a runtime gets none from an
    /// install (the first runtime is the operator's choice, CPU or GPU), and one
    /// that has any gets the pinned one.
    #[tokio::test]
    async fn with_if_installed_a_machine_without_a_runtime_downloads_nothing() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let home = Home::new("runtime-if-installed");
        let _h = ScopedEnv::set("HOME", &home.root.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &home.root.display().to_string());
        let proxy = DeadProxy::start().await;
        let (_, lib_name) = host_cpu_runtime();
        let ask = RuntimeArgs {
            if_installed: true,
            ..RuntimeArgs::default()
        };

        download_runtime(&ask)
            .await
            .expect("with no runtime there is nothing to update, and that is not an error");
        assert_eq!(
            proxy.hits(),
            0,
            "an install that finds no runtime must not download 450 MB nobody asked for"
        );

        put(
            &home.roots.preferred.join("onnxruntime").join(lib_name),
            b"runtime of an older version",
        );
        let updating = download_runtime(&ask).await;
        assert!(
            updating.is_err() && proxy.hits() >= 1,
            "with a runtime of unknown version in place the update is attempted: {updating:?}"
        );
    }

    /// CI-D5b. `--cuda` is the one flag that takes a value, so the reading of
    /// the arguments is judged on its own: a typo of `--gpu` that downloads the
    /// CPU package in silence is the failure.
    #[test]
    fn the_runtime_flags_are_read_and_the_ones_that_make_no_sense_are_refused() {
        let args = |list: &[&str]| -> Vec<String> { list.iter().map(|a| a.to_string()).collect() };

        let none = parse_runtime_args(&args(&[])).expect("no flags is the CPU runtime");
        assert_eq!(
            (none.gpu, none.cuda, none.if_installed),
            (false, None, false)
        );
        let gpu = parse_runtime_args(&args(&["--gpu"])).expect("--gpu");
        assert_eq!((gpu.gpu, gpu.cuda), (true, None));
        let forced = parse_runtime_args(&args(&["--gpu", "--cuda", "12"])).expect("--cuda 12");
        assert_eq!((forced.gpu, forced.cuda), (true, Some(12)));
        let order = parse_runtime_args(&args(&["--cuda", "13", "--gpu"]))
            .expect("the order of the flags does not matter");
        assert_eq!((order.gpu, order.cuda), (true, Some(13)));
        let update = parse_runtime_args(&args(&["--if-installed"])).expect("--if-installed");
        assert!(update.if_installed && !update.gpu);

        for (flags, words) in [
            (vec!["--cuda", "12"], vec!["--gpu"]),
            (vec!["--gpu", "--cuda", "11"], vec!["12", "13"]),
            (vec!["--gpu", "--cuda"], vec!["--cuda"]),
            (vec!["--gpus"], vec!["--gpus"]),
        ] {
            let said = format!(
                "{:#}",
                parse_runtime_args(&args(&flags)).expect_err("these flags are refused")
            );
            for word in words {
                assert!(
                    said.contains(word),
                    "{flags:?}: the refusal names `{word}`: {said}"
                );
            }
        }
    }

    /// The fixture of a swap cut between its two renames: the old runtime sits in
    /// `<leaf>.previous` with a library of the operator's beside it, there is no
    /// leaf, and the new package is whole in staging.
    fn a_swap_cut_between_its_two_renames(home: &Home) -> (PathBuf, PathBuf, PathBuf) {
        let leaf = home.root.join("onnxruntime");
        let previous = sibling(&leaf, "previous");
        let staging = sibling(&leaf, "downloading");
        put(&previous.join("libonnxruntime.so"), b"old core 1.26.0");
        put(
            &previous.join("libonnxruntime_providers_cuda.so"),
            b"old provider",
        );
        put(&previous.join("libcudart.so.12"), b"cudart of the operator");
        put(&staging.join("libonnxruntime.so"), b"new core 1.31.1");
        put(
            &staging.join("runtime.json"),
            record_json("1.31.1", "cpu", "x").as_bytes(),
        );
        assert!(
            !leaf.exists(),
            "the fixture is the machine a process killed between the renames leaves"
        );
        (leaf, previous, staging)
    }

    /// CI-D5c (A.1a). `swap_runtime` used to delete `<leaf>.previous` first and ask
    /// afterwards whether there was a leaf: on a machine cut between the renames
    /// that was the only copy of the runtime. Now it comes back before anything
    /// is deleted, and a new package that does not load leaves it exactly as it was.
    #[test]
    fn a_previous_runtime_with_no_leaf_comes_back_before_anything_is_deleted() {
        let home = Home::new("swap-cut-refused");
        let (leaf, previous, staging) = a_swap_cut_between_its_two_renames(&home);
        let old = snapshot(&previous);
        let new = snapshot(&staging);

        let error = swap_runtime(
            &staging,
            &leaf,
            &|_| Err(anyhow::anyhow!("the new core does not load")),
            &really_rename,
        )
        .expect_err("a runtime that does not load does not replace the old one");
        assert!(
            format!("{error:#}").contains("the new core does not load"),
            "the error carries the cause the check gave: {error:#}"
        );
        assert_eq!(
            snapshot(&leaf),
            old,
            "the leaf has exactly the files of `.previous`: the only copy of the runtime was \
             deleted before the swap looked for a leaf, and a machine cut here had none left"
        );
        assert!(
            !previous.exists(),
            "the aside copy became the runtime again"
        );
        assert_eq!(
            snapshot(&sibling(&leaf, "failed")),
            new,
            "the package that did not load is kept to be looked at"
        );

        let home = Home::new("swap-cut-accepted");
        let (leaf, previous, staging) = a_swap_cut_between_its_two_renames(&home);
        let new = snapshot(&staging);
        swap_runtime(&staging, &leaf, &|_| Ok(()), &really_rename)
            .expect("a runtime that loads replaces the one that came back");
        let mut want = new;
        want.insert(
            PathBuf::from("libcudart.so.12"),
            Some(b"cudart of the operator".to_vec()),
        );
        assert_eq!(
            snapshot(&leaf),
            want,
            "the new core, the new record and the library of the operator, and nothing of the \
             old package"
        );
        assert!(!previous.exists(), "nothing is left aside once it loaded");
    }

    /// CI-D5c (A.1b). The next run finds the leaf missing and `.previous` there.
    /// With `--if-installed`, which is what `npm install` runs, it used to say
    /// «no hay ninguno» and exit 0: a machine without a runtime for good.
    #[tokio::test]
    async fn if_installed_finishes_a_swap_cut_between_its_two_renames() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let home = Home::new("runtime-cut-swap");
        let _h = ScopedEnv::set("HOME", &home.root.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &home.root.display().to_string());
        let proxy = DeadProxy::start().await;
        let (_, lib_name) = host_cpu_runtime();
        let leaf = home.roots.preferred.join("onnxruntime");
        let previous = sibling(&leaf, "previous");
        put(&previous.join(lib_name), b"runtime of an older version");
        put(&previous.join("libcudart.so.12"), b"cudart of the operator");
        let old = snapshot(&previous);
        let ask = RuntimeArgs {
            if_installed: true,
            ..RuntimeArgs::default()
        };

        let updating = download_runtime(&ask).await;

        let attempted = proxy.hits();
        let said = format!("{updating:?}");
        assert!(
            updating.is_err() && attempted >= 1,
            "the runtime that came back is one of unknown version, and `--if-installed` updates \
             it: answering «no hay ninguno» over a leaf that was only renamed away leaves the \
             machine without a runtime ({attempted} requests): {said}"
        );
        assert!(
            said.contains("descargando el runtime"),
            "the failure is the one of the download, not of the recovery: {said}"
        );
        assert_eq!(
            snapshot(&leaf),
            old,
            "after the failed update the leaf has the runtime it had, file for file"
        );
        assert!(!previous.exists(), "and nothing is left aside");
    }

    /// CI-D5c (A.2). The hint of a provider that would not start says to put the
    /// CUDA libraries «junto al runtime de ONNX», and the replacement used to
    /// delete the whole old directory: whoever followed it lost them on the first
    /// update. What is not the package moves to the new directory; what is the
    /// old package never does, or the mix of two versions comes back. The
    /// fixture holds a library of each system on purpose: the rule is by name.
    #[test]
    fn what_the_operator_put_beside_the_runtime_survives_its_replacement() {
        let home = Home::new("swap-keeps-the-operators-files");
        let (leaf, staging) = old_and_new_runtime(&home);
        put(
            &leaf.join("runtime.json"),
            record_json("1.26.0", "cuda12", "old").as_bytes(),
        );
        put(&leaf.join("libcudart.so.12"), b"cudart of the operator");
        put(&leaf.join("cudnn64_9.dll"), b"cudnn of the operator");
        let new = snapshot(&staging);

        swap_runtime(&staging, &leaf, &|_| Ok(()), &really_rename)
            .expect("a runtime that loads replaces the old one");

        let mut want = new;
        want.insert(
            PathBuf::from("libcudart.so.12"),
            Some(b"cudart of the operator".to_vec()),
        );
        want.insert(
            PathBuf::from("cudnn64_9.dll"),
            Some(b"cudnn of the operator".to_vec()),
        );
        assert_eq!(
            snapshot(&leaf),
            want,
            "the new core, its record, and the two libraries of the operator with their bytes; \
             the old provider and the old record are the old package and do not pass"
        );
        assert!(
            !leaf.join("libonnxruntime_providers_cuda.so").exists(),
            "a provider of 1.26.0 beside the core of 1.31.1 is the mix the swap exists to prevent"
        );
        assert!(!sibling(&leaf, "previous").exists());

        let home = Home::new("swap-keeps-previous-on-a-clash");
        let (leaf, staging) = old_and_new_runtime(&home);
        let previous = sibling(&leaf, "previous");
        put(&leaf.join("libcudart.so.12"), b"cudart of the operator");
        put(&staging.join("libcudart.so.12"), b"cudart of the package");

        swap_runtime(&staging, &leaf, &|_| Ok(()), &really_rename)
            .expect("a file that cannot move does not undo a runtime that loaded");

        assert!(
            previous.exists(),
            "a file of the operator that could not move to the new directory is not deleted \
             with the old one"
        );
        assert_eq!(
            read(&previous.join("libcudart.so.12")),
            b"cudart of the operator",
            "it stays where it was, with its bytes"
        );
        assert_eq!(
            read(&leaf.join("libcudart.so.12")),
            b"cudart of the package",
            "and the one the new package brought is not overwritten"
        );
        let said = retire_previous(&previous, &leaf)
            .expect("the next attempt says again what stays, and where");
        assert!(
            said.contains("libcudart.so.12") && said.contains(&previous.display().to_string()),
            "the message names the file and the directory it is in: {said}"
        );

        let again = sibling(&leaf, "downloading");
        put(&again.join("libonnxruntime.so"), b"newer core");
        let before = (snapshot(&leaf), snapshot(&again), snapshot(&previous));
        let refused = swap_runtime(&again, &leaf, &|_| Ok(()), &really_rename).expect_err(
            "a swap does not start over a directory that still holds the operator's files",
        );
        assert!(
            format!("{refused:#}").contains("libcudart.so.12"),
            "the refusal names the file that blocks it: {refused:#}"
        );
        assert_eq!(
            (snapshot(&leaf), snapshot(&again), snapshot(&previous)),
            before,
            "a refused swap touches nothing: neither the runtime, nor the package waiting, nor \
             the files of the operator"
        );
    }

    /// CI-D5c (A.3, A.5). An install with a CUDA provider and no record predates
    /// the record, and the installer of then fetched `…-gpu-<v>`, which is CUDA
    /// 12: the libraries beside it are the ones of 12, so an unattended update
    /// stays on 12 whatever the driver says. The driver is read only when its
    /// answer decides: the table below counts the reads.
    #[test]
    fn an_unrecorded_gpu_install_stays_on_cuda_12_and_the_driver_is_read_only_when_it_decides() {
        let reads = std::cell::Cell::new(0usize);
        let driver_13_card_ada = || {
            reads.set(reads.get() + 1);
            Some(CudaProbe {
                driver: 13030,
                compute: Some((8, 9)),
            })
        };
        let ask = |gpu: bool, cuda: Option<u8>| RuntimeArgs {
            gpu,
            cuda,
            if_installed: false,
        };
        let gpu_without_record = Installed::Unrecorded { gpu_provider: true };
        let cpu_without_record = Installed::Unrecorded {
            gpu_provider: false,
        };
        let nothing = Installed::Nothing;

        for (row, asked, installed, want, expected_reads) in [
            (
                "no record, a CUDA provider, no --gpu",
                ask(false, None),
                &gpu_without_record,
                Some(Flavor::Cuda12),
                0,
            ),
            (
                "no record, a CUDA provider, --gpu --cuda 12",
                ask(true, Some(12)),
                &gpu_without_record,
                Some(Flavor::Cuda12),
                0,
            ),
            (
                "--gpu --cuda 13",
                ask(true, Some(13)),
                &nothing,
                Some(Flavor::Cuda13),
                1,
            ),
            (
                "--gpu alone",
                ask(true, None),
                &nothing,
                Some(Flavor::Cuda13),
                1,
            ),
            (
                "no record, a CUDA provider, --gpu alone",
                ask(true, None),
                &gpu_without_record,
                Some(Flavor::Cuda13),
                1,
            ),
            (
                "no record, no provider, no --gpu",
                ask(false, None),
                &cpu_without_record,
                None,
                0,
            ),
            (
                "nothing installed, no --gpu",
                ask(false, None),
                &nothing,
                None,
                0,
            ),
        ] {
            reads.set(0);
            let got = resolve_gpu(&asked, installed, &driver_13_card_ada)
                .unwrap_or_else(|e| panic!("{row}: {e:#}"));
            assert_eq!(got, want, "{row}");
            assert_eq!(
                reads.get(),
                expected_reads,
                "{row}: reads of the driver. `--cuda 12` and an unrecorded GPU install are \
                 answered without it, and `cuInit` on a machine with a card is not free"
            );
        }
    }

    /// CI-D5c (A.4a). A connection that is open and silent is the one failure a
    /// download has no answer to: `npm install` would wait on it for ever. The
    /// listener accepts and keeps the socket, so the wait is the client's. The 20
    /// seconds of the test only end it when the limit is missing.
    #[tokio::test]
    async fn a_download_that_stops_answering_is_cut_and_leaves_no_part_file() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let _no_proxy: Vec<ScopedEnv> = ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
            .iter()
            .map(|name| ScopedEnv::cleared(name))
            .collect();
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback port is free");
        let url = format!(
            "http://{}/runtime.tgz",
            silent.local_addr().expect("it is bound")
        );
        let holding = tokio::spawn(async move {
            let mut sockets = Vec::new();
            while let Ok((stream, _)) = silent.accept().await {
                sockets.push(stream);
            }
        });
        let root = scratch_root("download-stalls");
        std::fs::create_dir_all(&root).expect("the test owns this directory");
        let dest = root.join("runtime.tgz");
        let limits = Timeouts {
            connect: std::time::Duration::from_millis(200),
            read: std::time::Duration::from_millis(200),
        };

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            download_to_with(&url, &dest, limits),
        )
        .await;
        holding.abort();

        let result = outcome.expect(
            "the download is still waiting for a peer that never answers: the client has no read \
             time limit, and only the 20 seconds of the test ended the wait",
        );
        assert!(
            result.is_err(),
            "a peer that never answers is an error, not a download"
        );
        assert!(
            !dest.exists() && !dest.with_extension("part").exists(),
            "nothing is left for the next run to take for a package: {:?}",
            snapshot(&root).keys().collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// CI-D5c (A.6). Two `models runtime` at once (two terminals, or `npm
    /// install` while the operator runs it) shared one staging directory and one
    /// leaf: the second deleted what the first was filling. The lock is a real
    /// one on a real file, and a second handle of this process conflicts with
    /// it as a second process does.
    #[tokio::test]
    async fn a_second_models_runtime_refuses_while_the_first_holds_the_lock_and_touches_nothing() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let home = Home::new("runtime-locked");
        let _h = ScopedEnv::set("HOME", &home.root.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &home.root.display().to_string());
        let proxy = DeadProxy::start().await;
        let (archive, lib_name) = host_cpu_runtime();
        let leaf = home.roots.preferred.join("onnxruntime");
        put(&leaf.join(lib_name), b"old core");
        put(
            &leaf.join("runtime.json"),
            record_json("1.26.0", "cpu", &archive).as_bytes(),
        );
        let staging = sibling(&leaf, "downloading");
        put(&staging.join("witness"), b"half a package, another run's");
        let lock_path = home.roots.preferred.join(RUNTIME_LOCK);
        let first = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .expect("the test owns this directory");
        first
            .try_lock()
            .expect("nobody else holds the lock of this home");
        let before = snapshot(&home.roots.preferred);

        let refused = download_runtime(&RuntimeArgs::default())
            .await
            .expect_err("a second run does not share the directory of the first");

        let said = format!("{refused:#}");
        assert!(
            said.contains(RUNTIME_LOCK) && said.contains("esperá"),
            "the error names the lock and says to wait: {said}"
        );
        assert_eq!(
            snapshot(&home.roots.preferred),
            before,
            "the runtime and the staging directory of the first run are exactly as they were, \
             witness file included"
        );
        assert_eq!(
            proxy.hits(),
            0,
            "a refused run makes no request: it would be downloading into a directory that is \
             not its own"
        );

        drop(first);
        let later = download_runtime(&RuntimeArgs::default()).await;
        assert!(
            later.is_err() && proxy.hits() >= 1,
            "once the first run lets go the lock is free again, and a run goes on to the \
             download ({} requests): {later:?}",
            proxy.hits()
        );
    }
}
