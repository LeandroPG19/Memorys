//! What decides the reranker's fixed batch shape: the explicit override, and
//! otherwise the placement the reranker is headed for.
//!
//! The assertions that name a device can only fail on a build that has `cuda`
//! or `directml`. `gpu::wants_gpu` returns false on the feature check before it
//! ever reads the variable, so on a CPU build the device knob is inert by
//! construction and every answer here is false. `run-all-tests.sh` compiles
//! this binary twice: once in the debug test step without the feature, which is
//! where these three run today, and once in release with `--features cuda`,
//! where `-- --ignored` selects only the two ignored tests below. Asking for
//! the feature and not passing `--ignored` is what puts the device half under
//! load.

static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

const FIXED_SHAPE: &str = "CUBA_RERANK_FIXED_SHAPE";
const PREFERRED_DEVICE: &str = "MEMORY_INDUSTRY_RERANK_DEVICE";
const LEGACY_DEVICE: &str = "CUBA_RERANK_DEVICE";

/// Everything `fixed_shape()` reads. The device pair belongs in the same list
/// because the default falls through to `gpu::wants_gpu`, which reads it: a
/// developer with `CUBA_RERANK_DEVICE=cpu` exported in their shell used to see
/// this file go red over a message about padding, which names neither variable.
const FIXED_SHAPE_ENV: [&str; 3] = [FIXED_SHAPE, PREFERRED_DEVICE, LEGACY_DEVICE];

/// The process-wide variables one test owns while it runs: serialised against
/// the rest of this binary, cleared on the way in, restored on the way out.
///
/// The restore lives in `Drop` and not on the last line of a test on purpose.
/// The previous version cleared `CUBA_RERANK_FIXED_SHAPE` after its assertions,
/// so that line was only reached when all of them held — a red test leaked the
/// variable into whatever ran next in this process.
struct OwnedEnvironment {
    restore: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _serialised: std::sync::MutexGuard<'static, ()>,
}

fn own_the_environment(names: &[&'static str]) -> OwnedEnvironment {
    let serialised = ENV_GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let restore: Vec<_> = names
        .iter()
        .map(|&name| (name, std::env::var_os(name)))
        .collect();
    for (name, _) in &restore {
        unsafe { std::env::remove_var(name) };
    }
    OwnedEnvironment {
        restore,
        _serialised: serialised,
    }
}

impl OwnedEnvironment {
    fn set(&self, name: &str, value: impl AsRef<std::ffi::OsStr>) {
        unsafe { std::env::set_var(name, value) };
    }

    fn clear(&self, name: &str) {
        unsafe { std::env::remove_var(name) };
    }
}

impl Drop for OwnedEnvironment {
    fn drop(&mut self) {
        for (name, previous) in &self.restore {
            unsafe {
                match previous {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

fn fixed_shape() -> bool {
    memory_industry::search::rerank::fixed_shape()
}

/// The runtime placement decision `fixed_shape()` defers to when nobody has
/// overridden it.
fn reranker_wants_the_gpu() -> bool {
    memory_industry::gpu::wants_gpu(memory_industry::gpu::Workload::Reranker)
}

#[test]
fn without_an_override_the_fixed_shape_follows_where_the_reranker_will_run() {
    let env = own_the_environment(&FIXED_SHAPE_ENV);

    assert_eq!(
        fixed_shape(),
        reranker_wants_the_gpu(),
        "padding to a constant 512 pays off only on GPU, where a new tensor shape forces a \
         kernel recompile; on CPU it just makes every batch bigger. So the default is the \
         placement decision itself. Asserting it against `cfg!(feature = \"cuda\")` measured \
         a different axis — how this was built, not where it will run — and the two agreed \
         only while the gate compiled this file without the feature and no device variable \
         was set"
    );

    for off in ["0", "off", "false"] {
        env.set(FIXED_SHAPE, off);
        assert!(
            !fixed_shape(),
            "`{FIXED_SHAPE}={off}` is an operator overruling the placement default, and it has \
             to win on any hardware"
        );
    }

    for on in ["1", "true"] {
        env.set(FIXED_SHAPE, on);
        assert!(
            fixed_shape(),
            "`{FIXED_SHAPE}={on}` has to win the same way in the other direction"
        );
    }
}

#[test]
fn the_device_variable_decides_the_default_under_either_spelling() {
    let env = own_the_environment(&FIXED_SHAPE_ENV);
    let with_no_device_named = fixed_shape();

    for device in [PREFERRED_DEVICE, LEGACY_DEVICE] {
        env.set(device, "cpu");
        assert!(
            !fixed_shape(),
            "`{device}=cpu` is how an operator keeps the reranker off the card, and padding \
             every batch to 512 is exactly the cost that buys nothing there. False in every \
             build: without a GPU feature `wants_gpu` never reaches the variable, and with one \
             the `cpu` arm answers false"
        );

        env.set(device, "gpu");
        assert_eq!(
            fixed_shape(),
            with_no_device_named,
            "the reranker is the one workload whose default placement already is the card, so \
             naming it in `{device}` has to read exactly like not naming it. Comparing two runs \
             of the same function rather than a constant keeps this true on a CPU build and on \
             a `--features cuda` one"
        );
        env.clear(device);
    }
}

#[test]
fn the_new_spelling_of_the_device_wins_over_the_old_one() {
    let env = own_the_environment(&FIXED_SHAPE_ENV);
    env.set(PREFERRED_DEVICE, "cpu");
    env.set(LEGACY_DEVICE, "gpu");

    assert!(
        !fixed_shape(),
        "`envs::alias` reads {PREFERRED_DEVICE} first and falls back to {LEGACY_DEVICE} only \
         when it is unset, so an operator who moved to the new name and left the old one in a \
         service file gets the new one. This is the direction worth pinning: with the \
         precedence intact it is false in every build, and the moment the two are swapped it \
         turns true on a GPU build"
    );
}

#[test]
fn is_configured_reports_whether_a_model_is_on_disk_without_loading_it() {
    let env = own_the_environment(&["CUBA_RERANKER_PATH"]);
    let missing = std::env::temp_dir().join("cuba-no-reranker-here");
    env.set("CUBA_RERANKER_PATH", &missing);
    assert!(
        !memory_industry::search::rerank::is_configured(),
        "a path with no model.onnx must not claim to be configured, or startup would warm \
         up something that cannot load"
    );
}

#[tokio::test]
#[ignore]
async fn warming_up_leaves_the_reranker_ready() {
    assert!(
        memory_industry::search::rerank::is_configured(),
        "no reranker model on disk: this suite measures the model path and cannot report on it"
    );
    let started = std::time::Instant::now();
    let warm = memory_industry::search::rerank::warm_up().await;
    assert!(warm, "a configured reranker must warm up successfully");
    assert!(
        memory_industry::search::rerank::enabled(),
        "after warm-up the reranker must report enabled, so searches take the model path"
    );
    eprintln!("warm-up took {:.2}s", started.elapsed().as_secs_f32());
}

#[tokio::test]
#[ignore]
async fn reranking_reorders_candidates_by_relevance() {
    assert!(
        memory_industry::search::rerank::is_configured(),
        "no reranker model on disk: this suite measures the model path and cannot report on it"
    );
    memory_industry::search::rerank::warm_up().await;
    assert!(
        memory_industry::search::rerank::enabled(),
        "the reranker is configured but did not load. That is the failure this test exists to \
         catch — the identity fallback returns the RRF order unchanged and every search looks \
         like it worked"
    );

    let query = "how does the REM consolidation cycle decay old memories";
    let candidates = vec![
        "Bees overwinter best when the hive is insulated and the entrance reduced.",
        "The REM consolidation cycle applies stratified exponential decay to observation \
         importance, with a half-life that depends on the observation type.",
        "Docker multi-stage builds keep the final image small by discarding build tooling.",
    ];

    // An explicit deadline, not the search budget. This test is about whether
    // the cross-encoder ranks the right passage first; it is not about whether
    // it can win the session permit inside 20 s while the warm-up test next to
    // it holds that permit for a 50-candidate batch. On CPU that batch takes
    // 45 seconds, and the two run in the same process.
    let scored = memory_industry::search::rerank::rerank_within(
        query,
        &candidates,
        std::time::Instant::now() + std::time::Duration::from_secs(600),
    )
    .await
    .expect("reranking");
    assert_eq!(scored.len(), candidates.len());
    assert_eq!(
        scored[0].0,
        1,
        "the passage that actually describes REM decay must rank first, got order {:?}",
        scored.iter().map(|(i, _)| *i).collect::<Vec<_>>()
    );
    assert!(
        scored[0].1 > scored[2].1,
        "the cross-encoder must separate the relevant passage from the irrelevant ones"
    );
}
