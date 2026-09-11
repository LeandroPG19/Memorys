use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::{Path, PathBuf};

const ORT_VERSION: &str = "1.26.0";

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

fn cache_root() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("no HOME/USERPROFILE — no sé dónde está la caché")?;
    let cache = PathBuf::from(home).join(".cache");
    let preferred = cache.join("memory-industry");
    let legacy = cache.join("cuba-memorys");
    if preferred.exists() || !legacy.exists() {
        Ok(preferred)
    } else {
        Ok(legacy)
    }
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    let what = args.first().map(String::as_str).unwrap_or("");
    let gpu = args.iter().any(|a| a == "--gpu");
    match what {
        "embed" | "embeddings" | "nli" | "reranker" => {
            download_model(&spec(what).unwrap()).await?;
        }
        "runtime" => {
            download_runtime(gpu).await?;
        }
        "all" => {
            download_runtime(gpu).await?;
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
         memory-industry los encuentra ahí solo — no hace falta setear env vars.\n\n\
           embed      multilingual-e5-small (384-d) — búsqueda semántica. ~113 MB\n\
           nli        mDeBERTa-v3-xnli — verify sin LLM. ~1.1 GB\n\
           reranker   bge-reranker-v2-m3 — reordena candidatos. ~4.5 GB (onnx + onnx_data)\n\
           runtime    libonnxruntime para tu plataforma. ~15 MB\n\
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
];

pub fn sha256_file(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
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

async fn download_model(spec: &ModelSpec) -> Result<()> {
    let dir = cache_root()?.join(spec.dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creando {}", dir.display()))?;

    println!(
        "→ {} ({} archivos) en {}",
        spec.hf_repo,
        spec.files.len(),
        dir.display()
    );
    for (remote, local) in spec.files {
        let dest = dir.join(local);
        if dest.exists() && std::fs::metadata(&dest)?.len() > 0 {
            println!("  ✓ {local} (ya está)");
            continue;
        }
        let url = format!(
            "https://huggingface.co/{}/resolve/main/{remote}",
            spec.hf_repo
        );
        print!("  ↓ {local} ... ");
        std::io::stdout().flush().ok();
        download_to(&url, &dest)
            .await
            .with_context(|| format!("descargando {local}"))?;
        verify_checksum(&dest, &format!("{}/{}", spec.dir, local))?;
        let mb = std::fs::metadata(&dest)?.len() / 1_048_576;
        println!("{mb} MB");
    }
    println!(
        "  listo. memory-industry lo encuentra en {} (o {}=<ruta>)",
        dir.display(),
        spec.env_hint
    );
    Ok(())
}

fn runtime_target(gpu: bool) -> Result<(String, &'static str, &'static str)> {
    let v = ORT_VERSION;
    let (archive, ext, lib) = match (std::env::consts::OS, std::env::consts::ARCH, gpu) {
        ("linux", "x86_64", false) => (
            format!("onnxruntime-linux-x64-{v}"),
            "tgz",
            "libonnxruntime.so",
        ),
        ("linux", "x86_64", true) => (
            format!("onnxruntime-linux-x64-gpu-{v}"),
            "tgz",
            "libonnxruntime.so",
        ),
        ("linux", "aarch64", _) => (
            format!("onnxruntime-linux-aarch64-{v}"),
            "tgz",
            "libonnxruntime.so",
        ),
        ("macos", "x86_64", _) => (
            format!("onnxruntime-osx-x86_64-{v}"),
            "tgz",
            "libonnxruntime.dylib",
        ),
        ("macos", "aarch64", _) => (
            format!("onnxruntime-osx-arm64-{v}"),
            "tgz",
            "libonnxruntime.dylib",
        ),
        ("windows", "x86_64", false) => {
            (format!("onnxruntime-win-x64-{v}"), "zip", "onnxruntime.dll")
        }
        ("windows", "x86_64", true) => (
            format!("onnxruntime-win-x64-gpu-{v}"),
            "zip",
            "onnxruntime.dll",
        ),
        (os, arch, _) => bail!(
            "no tengo el runtime prearmado para {os}/{arch}. Instalá onnxruntime {v} a mano \
             y apuntá ORT_DYLIB_PATH a la librería."
        ),
    };
    let ext: &'static str = ext;
    let lib: &'static str = lib;
    Ok((archive, ext, lib))
}

async fn download_runtime(gpu: bool) -> Result<()> {
    let (archive_stem, ext, lib_name) = runtime_target(gpu)?;
    let dir = cache_root()?.join("onnxruntime");
    std::fs::create_dir_all(&dir)?;
    let lib_dest = dir.join(lib_name);

    if !gpu && lib_dest.exists() && std::fs::metadata(&lib_dest)?.len() > 0 {
        println!("→ runtime: {lib_name} ya está en {}", dir.display());
        return Ok(());
    }

    let url = format!(
        "https://github.com/microsoft/onnxruntime/releases/download/v{ORT_VERSION}/{archive_stem}.{ext}"
    );
    println!(
        "→ runtime {}({}/{}) desde {url}",
        if gpu { "GPU " } else { "" },
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    let archive_path = dir.join(format!("{archive_stem}.{ext}"));
    print!("  ↓ descargando ... ");
    std::io::stdout().flush().ok();
    download_to(&url, &archive_path)
        .await
        .context("descargando el runtime")?;
    println!("{} MB", std::fs::metadata(&archive_path)?.len() / 1_048_576);

    print!("  ⇢ extrayendo ... ");
    std::io::stdout().flush().ok();
    let extracted = extract_runtime(&archive_path, ext, lib_name, &dir, gpu)
        .with_context(|| format!("extrayendo el runtime de {}", archive_path.display()))?;
    std::fs::remove_file(&archive_path).ok();
    println!("ok ({} librerías)", extracted.len());
    for name in &extracted {
        println!("     {name}");
    }
    println!(
        "  listo. memory-industry lo encuentra en {} (o ORT_DYLIB_PATH=<ruta>)",
        lib_dest.display()
    );
    if gpu {
        println!(
            "  nota GPU: el provider CUDA necesita las libs de CUDA 12 + cuDNN 9 accesibles.\n\
             \x20      si el doctor cae a CPU, agregá sus rutas a LD_LIBRARY_PATH (Linux) o PATH (Windows)."
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

async fn download_to(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("memory-industry/", env!("CARGO_PKG_VERSION")))
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
