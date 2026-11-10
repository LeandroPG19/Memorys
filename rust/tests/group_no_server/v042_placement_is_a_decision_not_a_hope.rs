//! Where the work lands is a decision, and a decision can be read on a machine
//! with no card.
//!
//! Nobody can prove here that a kernel executed on a GPU — the merge box has no
//! GPU and never will. What broke in the field was never the kernel: it was the
//! *reason* the daemon gave, which sent an operator to fix the wrong thing. So
//! this file judges `gpu::status_from`, which takes the three facts about a
//! machine as arguments instead of measuring them.
//!
//! There is deliberately no `cfg!` in this file. Every branch a CUDA build
//! takes is executed on a CPU build, because the branch is chosen by an
//! argument. Running it with `--features cuda` would change nothing, and that
//! is the point of the split it covers.
//!
//! The half that *is* feature-dependent — `wants_gpu`, which short-circuits on
//! the compiled feature before it reads a variable — lives in
//! `gpu::placement_tests` in the library, where `session::GLOBAL_STATE_GUARD`
//! and `envs::ScopedEnv` exist. Both are `#[cfg(test)]` and are not reachable
//! from an integration test, and a hand-rolled save/restore beside them would
//! be a second serialisation that does not serialise against the first.

use memory_industry::gpu::{GpuStatus, status_from};

/// The provider name is an argument, so one name is enough to cover the
/// compiled half: a second one would only change the word inside the message.
const PROVIDER: &str = "cuda";

/// `(built with a GPU provider, provider libraries beside the runtime, card)`
/// with its verdict, for all eight machines.
fn every_machine_state() -> Vec<(Option<&'static str>, bool, bool, GpuStatus)> {
    let mut rows = Vec::with_capacity(8);
    for compiled in [None, Some(PROVIDER)] {
        for runtime_gpu in [false, true] {
            for device_present in [false, true] {
                rows.push((
                    compiled,
                    runtime_gpu,
                    device_present,
                    status_from(compiled, runtime_gpu, device_present),
                ));
            }
        }
    }
    rows
}

fn read(relative: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

#[test]
fn every_machine_state_reads_differently_from_every_other() {
    let rows = every_machine_state();
    assert_eq!(
        rows.len(),
        8,
        "eight combinations and no exceptions left. Two of them used to be excused as \
         unobservable, because a build with no GPU feature reported the provider libraries \
         absent without looking; it looks now, so a count under eight here means a row was \
         dropped rather than answered"
    );

    for (index, (compiled, runtime_gpu, device_present, status)) in rows.iter().enumerate() {
        for (other_compiled, other_runtime, other_device, other) in rows.iter().skip(index + 1) {
            assert_ne!(
                status.detail, other.detail,
                "two machines that need different fixes say the same sentence: \
                 (compiled={compiled:?} runtime_gpu={runtime_gpu} device={device_present}) and \
                 (compiled={other_compiled:?} runtime_gpu={other_runtime} \
                 device={other_device}) both report «{}». That is the defect this file exists \
                 for: the operator reads the reason, and a reason shared by two causes sends \
                 half of them to change something that was never wrong",
                status.detail
            );
        }
    }
}

#[test]
fn a_build_without_gpu_support_says_whether_the_runtime_is_already_on_disk() {
    let only_the_build_left = status_from(None, true, true);
    let both_still_missing = status_from(None, false, true);

    assert_ne!(
        only_the_build_left.detail, both_still_missing.detail,
        "these two read identically until this release, because the arm that answers them \
         never looked for the provider libraries. An operator with a card could not tell \
         whether `build-gpu.sh` was the last step or the first of two, and the only way to \
         find out was to do both"
    );
    assert!(
        only_the_build_left.detail.contains("runtime GPU"),
        "the row where the runtime is already down has to say so, or the sentences differ \
         without the difference being the one that saves the trip: {}",
        only_the_build_left.detail
    );
    assert!(
        !both_still_missing.detail.contains("runtime"),
        "and the row where it is not must not mention it, or both sentences claim the same \
         thing about disk and the operator is back to guessing: {}",
        both_still_missing.detail
    );
    assert_eq!(
        only_the_build_left.hint.as_deref(),
        Some("./scripts/build-gpu.sh"),
        "a better diagnosis with no exit is worse than the collapsed one. The build is still \
         what this machine is missing, and the hint is still where the operator goes"
    );
}

#[test]
fn a_machine_missing_both_the_runtime_and_the_card_is_not_sent_to_download_a_runtime() {
    let nothing_at_all = status_from(Some(PROVIDER), false, false);
    let card_without_runtime = status_from(Some(PROVIDER), false, true);

    assert_ne!(
        nothing_at_all.detail, card_without_runtime.detail,
        "both used to report «the runtime installed is the CPU one». On a machine that also \
         has no card that is true and useless: the operator downloads a GPU runtime, restarts, \
         and only then learns there is nothing to run it on. Two causes, one sentence, two trips"
    );

    let hint = nothing_at_all
        .hint
        .expect("a degraded machine has to carry the next step, not just the diagnosis");
    assert!(
        hint.contains("nvidia-smi"),
        "the hint for a machine with neither has to name the card as well as the runtime, or \
         it is the same one-trip-at-a-time advice under a new sentence: {hint}"
    );
}

#[test]
fn degraded_is_true_exactly_when_the_machine_cannot_do_what_the_binary_was_built_for() {
    for (compiled, runtime_gpu, device_present, status) in every_machine_state() {
        let expected = match compiled {
            // Built for the card and not getting it.
            Some(_) => !(runtime_gpu && device_present),
            // Built for the CPU: only a wasted card is worth flagging. A plain
            // CPU machine reporting degraded would light up `doctor` on every
            // install that never wanted a GPU, and a warning everybody has
            // learned to ignore is how the real one gets missed.
            //
            // `runtime_gpu` is measured on this branch now and still does not
            // enter: provider libraries on disk with no card are unused bytes,
            // not a degradation. The exit `doctor` would offer is
            // `./scripts/build-gpu.sh`, which builds --features cuda, so
            // sending a machine nvidia-smi cannot see there is the wrong trip
            // in the other direction. It changes the *sentence*, which is what
            // the operator reads, and not the flag, which is what `doctor`,
            // `/health` and `mode::rerank_gpu_active` branch on.
            None => device_present,
        };
        assert_eq!(
            status.degraded, expected,
            "compiled={compiled:?} runtime_gpu={runtime_gpu} device={device_present} → \
             «{}»",
            status.detail
        );
    }
}

#[test]
fn every_answer_that_is_not_the_card_names_the_cpu_the_work_landed_on() {
    for (compiled, runtime_gpu, device_present, status) in every_machine_state() {
        if compiled.is_some() && runtime_gpu && device_present {
            assert!(
                status.detail.starts_with(PROVIDER),
                "the one healthy answer has to name the provider it is using, or `doctor` \
                 reports a device without saying which: {}",
                status.detail
            );
            continue;
        }
        assert!(
            status.detail.to_lowercase().contains("cpu"),
            "scripts/gpu-placement-check.sh fails the gate when a machine with no usable GPU \
             path reports a detail that never names the CPU it is actually running on. \
             compiled={compiled:?} runtime_gpu={runtime_gpu} device={device_present} → {}",
            status.detail
        );
    }
}

#[test]
fn the_gate_step_self_tests_against_sentences_this_code_still_produces() {
    let script = read("scripts/gpu-placement-check.sh");

    // The whole sentence, for the four the script pins verbatim. The last one
    // is the state the script could not see before: its `machine_can_gpu` fused
    // the provider libraries and the card into one bit, so a machine with the
    // runtime down and no card was indistinguishable from one with neither.
    for produced in [
        status_from(Some(PROVIDER), true, false).detail,
        status_from(Some(PROVIDER), false, true).detail,
        status_from(None, false, false).detail,
        status_from(None, true, false).detail,
    ] {
        assert!(
            script.contains(&produced),
            "gpu-placement-check.sh --self-test proves its guards can still fail by feeding \
             them what `doctor` really prints. It no longer has «{produced}» in it, so it is \
             now proving that on a sentence nothing produces — a self-test that passes over \
             fixtures the code stopped emitting is the guard quietly retiring itself"
        );
    }

    // The healthy one ends in `placement_summary()`, which is a function of the
    // environment, so only its fixed head can be pinned.
    let healthy = status_from(Some(PROVIDER), true, true).detail;
    let head = healthy
        .split_once("colocación:")
        .map(|(before, _)| format!("{before}colocación:"))
        .expect("the healthy answer carries the placement it decided");
    assert!(
        script.contains(&head),
        "the script's `gpu_live` fixture no longer starts like the real answer: «{head}»"
    );
}

#[test]
fn every_hint_names_the_same_binary() {
    let mut with_a_hint = 0;

    for (compiled, runtime_gpu, device_present, status) in every_machine_state() {
        // Presence anchor, and not a formality in either direction. doctor.rs
        // reads `Some(hint) if gpu.degraded` and falls through to `Check::ok`
        // for everything else, so a degraded status that lost its hint is
        // served as healthy — and the loop below would walk straight over it
        // having asserted nothing at all.
        assert_eq!(
            status.hint.is_some(),
            status.degraded,
            "compiled={compiled:?} runtime_gpu={runtime_gpu} device={device_present}: a \
             degraded machine has to carry the next step, and a healthy one has none to \
             carry. `doctor` decides warn against ok on exactly this, so the hint is not \
             decoration: without it a machine that cannot use its card reports ok"
        );

        let Some(hint) = status.hint else {
            continue;
        };
        with_a_hint += 1;
        assert!(
            !hint.contains("cuba-memorys"),
            "two hints six lines apart naming the binary differently is how an operator ends \
             up not knowing which of the two commands is the real one. And reading the code \
             is not what catches it: gpu-placement-check.sh already fed «memory-industry \
             models runtime --gpu» to this very case in its --self-test while this function \
             still said «cuba-memorys», so the guard and the product had been disagreeing \
             with nothing putting them side by side. Got: {hint}"
        );
    }

    assert!(
        with_a_hint > 0,
        "not one of the eight states carried a hint, so the check above judged nothing. A \
         ratchet that walks an empty loop retires itself without anybody editing it"
    );
}
