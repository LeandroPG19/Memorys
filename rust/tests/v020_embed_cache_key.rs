#[test]
fn the_cache_key_changes_with_the_model_so_a_switch_cannot_serve_stale_vectors() {
    unsafe { std::env::set_var("CUBA_EMBED_MODEL", "bge-m3") };
    let a = memory_industry::embeddings::onnx::model_fingerprint();

    unsafe { std::env::set_var("CUBA_EMBED_MODEL", "e5-small") };
    let b = memory_industry::embeddings::onnx::model_fingerprint();

    assert_ne!(
        a, b,
        "the fingerprint must follow CUBA_EMBED_MODEL. The cache used to key on prefix+text \
         alone, so switching models served vectors from the previous model — silently, and \
         for the whole TTL. Those vectors are not comparable with what is stored."
    );

    unsafe { std::env::remove_var("CUBA_EMBED_MODEL") };
    let fallback = memory_industry::embeddings::onnx::model_fingerprint();
    assert!(
        !fallback.is_empty(),
        "an unset model must still produce a stable key component, not an empty one"
    );
}
