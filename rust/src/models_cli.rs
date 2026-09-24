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
    // The outer context stays. `envs::home()` names both variables and talks
    // about where an MCP client looks for a config, which does not say WHAT
    // could not be located here; without this line the operator gets an error
    // that only blames the environment.
    let cache = crate::envs::home()
        .context("no sé dónde está la caché")?
        .join(".cache");
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
         MemoryIndustry los encuentra ahí solo — no hace falta setear env vars.\n\n\
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
        "  listo. MemoryIndustry lo encuentra en {} (o {}=<ruta>)",
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
        "  listo. MemoryIndustry lo encuentra en {} (o ORT_DYLIB_PATH=<ruta>)",
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
}
