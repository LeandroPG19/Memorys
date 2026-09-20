use std::env;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Local,
    Red,
    Completo,
}

pub fn active() -> Mode {
    match env::var("CUBA_MODE")
        .unwrap_or_default()
        .trim()
        .to_lowercase()
        .as_str()
    {
        "red" | "cloud" | "nube" => Mode::Red,
        "completo" | "full" | "complete" => Mode::Completo,
        _ => Mode::Local,
    }
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Local => "local",
            Mode::Red => "red",
            Mode::Completo => "completo",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Mode::Local => "BD local (Docker) · modelos locales · sin red saliente",
            Mode::Red => {
                "BD compartida en la nube (TLS) · procedencia por nodo · sync entre máquinas"
            }
            Mode::Completo => "todo activado · reranker (GPU si hay) · cuba_docs",
        }
    }

    pub fn is_cloud(self) -> bool {
        match self {
            Mode::Red => true,
            Mode::Completo => env::var("DATABASE_URL")
                .map(|u| !u.is_empty())
                .unwrap_or(false),
            Mode::Local => false,
        }
    }

    pub fn docs_default(self) -> bool {
        matches!(self, Mode::Completo)
    }

    pub fn rerank_default(self) -> bool {
        rerank_default_for(self, rerank_gpu_active())
    }
}

fn rerank_default_for(mode: Mode, gpu_active: bool) -> bool {
    matches!(mode, Mode::Completo) || gpu_active
}

fn gpu_really_active(compiled_with_gpu: bool, gpu_degraded: bool, placed_on_gpu: bool) -> bool {
    compiled_with_gpu && !gpu_degraded && placed_on_gpu
}

static RERANK_GPU_ACTIVE: OnceLock<bool> = OnceLock::new();

fn rerank_gpu_active() -> bool {
    *RERANK_GPU_ACTIVE.get_or_init(|| {
        gpu_really_active(
            cfg!(any(feature = "cuda", feature = "directml")),
            crate::gpu::status().degraded,
            crate::gpu::wants_gpu(crate::gpu::Workload::Reranker),
        )
    })
}

pub fn env_toggle(name: &str) -> Option<bool> {
    let v = env::var(name).ok()?;
    let v = v.trim().to_lowercase();
    if v.is_empty() {
        return None;
    }
    Some(matches!(
        v.as_str(),
        "1" | "true" | "on" | "yes" | "sí" | "si"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completo_turns_capabilities_on_others_leave_them_off() {
        assert!(Mode::Completo.docs_default());
        assert!(!Mode::Local.docs_default());

        // Completo is an operator's explicit choice, so it does not depend on
        // what hardware is underneath.
        assert!(Mode::Completo.rerank_default());

        // Local and Red defer to the hardware. That is the contract, and
        // stating it this way is true on a CPU build and on a GPU one.
        //
        // These used to assert a plain `false`, which was green only because
        // every `cargo test` in this gate compiles without cuda. The release
        // matrix builds Windows with `docs,cuda,directml`, and on a machine
        // with a working card this test went red for being right - measured
        // here on 2026-09-20 with `cargo test --lib --features cuda`.
        for mode in [Mode::Local, Mode::Red] {
            assert_eq!(
                mode.rerank_default(),
                rerank_gpu_active(),
                "{mode:?} follows the card, and this build says the reranker is {}on it",
                if rerank_gpu_active() { "" } else { "not " }
            );
        }

        assert!(Mode::Red.is_cloud());
        assert!(!Mode::Local.is_cloud());
    }

    #[test]
    fn a_real_gpu_turns_rerank_on_even_outside_completo_mode() {
        assert!(
            rerank_default_for(Mode::Local, true),
            "a genuinely active GPU provider must turn rerank on by default even outside \
             completo mode — tying the default to hardware instead of to a mode flag is \
             the whole point of this change"
        );
        assert!(rerank_default_for(Mode::Red, true));
        assert!(
            !rerank_default_for(Mode::Local, false),
            "CPU reranking costs 60-110s per search (measured, README) and blows the \
             request budget — it has to stay off unless asked for explicitly"
        );
        assert!(
            rerank_default_for(Mode::Completo, false),
            "CUBA_MODE=completo is an explicit operator choice and must win even with no GPU"
        );
    }

    #[test]
    fn gpu_really_active_requires_both_a_gpu_build_and_a_working_device() {
        assert!(
            gpu_really_active(true, false, true),
            "compiled with GPU support and the runtime reports no degradation — that is \
             what an active GPU looks like"
        );
        assert!(
            !gpu_really_active(true, true, true),
            "compiled with GPU support but degraded (CPU-only runtime, or no NVIDIA card \
             detected) — still no real GPU underneath"
        );
        assert!(
            !gpu_really_active(false, false, true),
            "a CPU-only build reports degraded=false for the ordinary case of having no \
             GPU at all to complain about — that false must never be read as 'GPU active', \
             or every plain CPU install would silently pay the 60-110s reranker tax"
        );
        assert!(!gpu_really_active(false, true, true));
        assert!(
            !gpu_really_active(true, false, false),
            "a GPU build on a machine with a working card still runs the reranker on the CPU \
             when CUBA_RERANK_DEVICE=cpu, and gpu::status() never looks at that variable: it \
             only checks that the provider file exists and that a driver is present. Reading \
             its degraded=false as «the reranker is on the GPU» turns such an install into a \
             106s rerank against a 20s budget, and the timeout does not even reclaim the work \
             — spawn_blocking is not cancellable and keeps holding the session mutex"
        );
    }
}
