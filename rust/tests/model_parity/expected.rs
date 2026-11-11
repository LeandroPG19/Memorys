//! CI-D4. What `tests/model_parity.rs` compares against: the outputs of `tokenizers` 0.21.4, `tiktoken-rs`
//! 0.7.0 and ONNX Runtime 1.26.0 on the inputs that file builds, taken from the binary of the day before
//! either library moved. Outputs only, by case name: the inputs live in `model_parity.rs`, and a case with
//! an output and no input (or the reverse) is red.
//!
//! Never edited by hand. A table left empty makes its test fail and print the block to paste over it
//! (`cargo test --workspace --test model_parity -- --nocapture`), between the lines
//! `--- paste into … ---` and `--- end of … ---`. The attribute lines above each constant belong to the
//! block: `rustfmt::skip` keeps the layout the test printed, and `approx_constant` is for a component that
//! happens to land near a mathematical constant, which says nothing about the vector.

/// One encoding: the case, then `ids`, `attention_mask` and `type_ids`.
pub type Encoded = (&'static str, &'static [u32], &'static [u32], &'static [u32]);

/// One vector of the embedder: the case, then its 384 components.
pub type Vector = (&'static str, &'static [f32]);

/// The variables that change a vector of the embedder and were set when the vectors were made, as
/// `(name, value)`; one that is not here was unset. `CUBA_EMBED_MODEL` is not one of them: it names the
/// model in the cache key and in the database and does not touch the numbers.
#[rustfmt::skip]
pub const EMBEDDER_ENV: &[(&str, &str)] = &[];

/// The embedder's tokenizer, with truncation to 512 and `BatchLongest`, as `onnx.rs` builds it.
#[rustfmt::skip]
pub const EMBEDDER_IDS: &[Encoded] = &[];

/// The reranker's tokenizer on four pairs in one `encode_batch`, as `rerank.rs` builds it.
#[rustfmt::skip]
pub const RERANKER_IDS: &[Encoded] = &[];

/// The NLI tokenizer, truncation to 512 and no padding, as `nli.rs` builds it.
#[rustfmt::skip]
pub const NLI_IDS: &[Encoded] = &[];

/// The vectors `embed_passage` and `embed` give, normalised, as the product computes them.
#[rustfmt::skip]
#[allow(clippy::approx_constant)]
pub const VECTORS: &[Vector] = &[];
