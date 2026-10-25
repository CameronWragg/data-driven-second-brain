# Research: Rust embedding runtime and model delivery

Resolves [#17](https://github.com/CameronWragg/data-driven-second-brain/issues/17). Researched 2026-10-25.

**Question.** Which Rust embedding runtime and delivery approach should the local embedding provider (SPEC §6.3) use? The candidates are `ort` (ONNX Runtime), `candle` and `fastembed-rs`. They are compared on support for the candidate models, static linking into one binary on Linux and macOS, binary size, CPU latency and how the model weights are delivered.

## Recommendation

1. **Runtime: `fastembed` 7.x on top of `ort` 2.0.0-rc.15, with ONNX Runtime statically linked from pyke's prebuilt binaries.** Configure it as `default-features = false, features = ["ort-download-binaries-rustls-tls"]`, with no `hf-hub` and no `image-models`. The provider loads models through `TextEmbedding::try_new_from_user_defined`, so fastembed never downloads anything itself. fastembed supplies tokenisation, pooling, normalisation and per-model output selection for all three candidate models. Using `ort` directly is the fallback if fastembed gets in the way, because fastembed is a thin layer over `ort`.
2. **Not `candle`.** It has no EmbeddingGemma implementation. It was also about 5–8× slower than ONNX Runtime on CPU in the only like-for-like benchmark found. Its one advantage is that it is pure Rust, with no C++ runtime.
3. **Weights: fetched by `brain init`, not bundled in the binary.** They are pinned to a Hugging Face revision and a SHA-256 hash, and stored outside the brain's git repo in a per-machine cache. `serve` and `reindex` fetch any model that is missing, so a new machine that only cloned the brain still works.

## Evidence

### Current versions (crates.io, 2026-10-25)

| Crate | Version | Notes |
|---|---|---|
| `ort` | 2.0.0-rc.15 (no stable 2.x) | Wraps ONNX Runtime 1.31.0 (`ort-sys/build/download/dist.tsv` URLs are `ms@1.31.0`) |
| `fastembed` | 7.5.0 (2026-10-23) | Pins `ort = "=2.0.0-rc.15"` and `tokenizers = "0.23.2"` |
| `candle-core` / `candle-transformers` | 0.11.0 (2026-06-26) | |

Source: the crates.io API; [fastembed-rs Cargo.toml](https://github.com/Anush008/fastembed-rs/blob/main/Cargo.toml); [ort-sys dist.tsv](https://github.com/pykeio/ort/blob/main/ort-sys/build/download/dist.tsv).

### Model support

| Model | fastembed (ONNX) | candle | Size of the ONNX weights |
|---|---|---|---|
| bge-small-en-v1.5 (33.4M params, 384-d, 512 tokens, MIT) | `EmbeddingModel::BGESmallENV15` and `BGESmallENV15Q`, CLS pooling | `models::bert` | fp32 133 MB; int8 33.8 MB (`Xenova/bge-small-en-v1.5`) |
| nomic-embed-text-v1.5 (0.1B, 768-d with Matryoshka 512/256/128/64, 8192 tokens, Apache-2.0) | `NomicEmbedTextV15` and `NomicEmbedTextV15Q`, mean pooling | `models::nomic_bert`, plus an example | fp32 547 MB; int8 137 MB |
| EmbeddingGemma-300m (768-d with Matryoshka 512/256/128, 2048 tokens, Gemma licence) | `EmbeddingGemma300M`, `…Q` and `…Q4`; reads the `sentence_embedding` output | **None.** `gemma3.rs` is a causal language model with a KV cache, not the bidirectional encoder with pooling and dense head that EmbeddingGemma needs | fp32 1,234 MB; q4 197 MB; int8 309 MB; plus a 20.3 MB `tokenizer.json` (`onnx-community/embeddinggemma-300m-ONNX`, using external `.onnx_data` files) |

Sources: [fastembed `models/text_embedding.rs`](https://github.com/Anush008/fastembed-rs/blob/main/src/models/text_embedding.rs); fastembed `text_embedding/impl.rs` (`get_default_pooling_method`); [candle-transformers models](https://github.com/huggingface/candle/tree/main/candle-transformers/src/models); the Hugging Face model-tree API for the file sizes; and the model cards for [bge-small](https://huggingface.co/BAAI/bge-small-en-v1.5), [nomic v1.5](https://huggingface.co/nomic-ai/nomic-embed-text-v1.5) and [EmbeddingGemma](https://huggingface.co/google/embeddinggemma-300m).

**Query and document labels.** Neither runtime adds labels. fastembed's README has callers write the prefix into the text themselves. Each model needs a different scheme, according to its model card:

| Model | Query | Document |
|---|---|---|
| bge-small-en-v1.5 | `Represent this sentence for searching relevant passages: ` | *none* ("no instruction needs to be added to passages") |
| nomic-embed-text-v1.5 | `search_query: ` (required) | `search_document: ` (required) |
| EmbeddingGemma | `task: search result \| query: ` | `title: {title \| "none"} \| text: ` |

So SPEC §6.2 step 3 is inaccurate (see *SPEC implications*).

**Bring-your-own weights.** `UserDefinedEmbeddingModel` takes the ONNX file, any external initialisers (needed for EmbeddingGemma's `.onnx_data`) and the tokenizer files as **bytes**, with an optional pooling method and output key. The provider can therefore read files from any path it chooses, and could even embed them with `include_bytes!`. Source: fastembed `src/text_embedding/init.rs`.

**EmbeddingGemma precision.** Its activations do not support float16, so it must run in fp32 or bf16 (model card). The `model_fp16`/`q4f16` ONNX variants should be avoided.

### Static linking and platforms

- **ort default behaviour.** It "download[s] statically-linked ONNX Runtime binaries built by pyke.io" at **build** time ([prebuilt-binaries docs](https://github.com/pykeio/ort/blob/main/docs/content/misc/prebuilt-binaries.mdx)). The build script emits `cargo:rustc-link-lib=static=onnxruntime` (`ort-sys/build/main.rs`). Nothing is fetched at run time. The archive is SHA-256-checked against `dist.tsv` and has SLSA attestation.
- **Not fully static.** The C++ standard library is still linked dynamically: `stdc++` on Linux and `c++` on macOS, plus the Foundation and CoreML frameworks on macOS (`ort-sys/build/static_link/mod.rs`). The docs say the Linux builds are compiled with Clang. The result is a single executable that depends only on the system libc and C++ runtime, which every mainstream Linux distro, WSL2 and macOS provide. There is no musl prebuilt, so a truly static musl binary would mean building ONNX Runtime ourselves.
- **Prebuilt CPU targets** (`dist.tsv`): `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` and `aarch64-apple-darwin`, plus Windows, Android and iOS. **There is no `x86_64-apple-darwin`** (Intel Mac), which would need a self-built ONNX Runtime (`ORT_LIB_PATH`) or `load-dynamic`.
- **CPU baseline.** "All x86-64 binaries are compiled with a baseline requirement of x86-64-v3", meaning Haswell (2013) or later, or any Ryzen. Older or low-end Intel chips such as Pentiums would fail.
- **TLS.** `download-binaries` needs a TLS feature, but that is only used by the build script. Choosing `rustls-tls` and dropping `hf-hub` avoids linking OpenSSL into the runtime binary. fastembed's *default* features (`ort-download-binaries-native-tls`, `hf-hub-native-tls`) would pull in native TLS.
- **candle** is pure Rust and links statically with no C++ runtime. On paper this is its strongest advantage.

### Binary size

No Rust toolchain was available here, so no `brain`-like binary was built. These are measured upper and lower bounds:

- pyke's `x86_64-unknown-linux-gnu` archive for ONNX Runtime 1.31.0 is 10.7 MB compressed. It holds a single `libonnxruntime.a` of **111.7 MB**, which is unlinked object code with symbols.
- Microsoft's official `libonnxruntime.so.1.31.0` for linux-x64, a fully linked CPU build, is **29.3 MB**.
- **Estimate:** statically linking ONNX Runtime adds roughly **25–30 MB** to a stripped release binary. ONNX Runtime registers its kernels statically, so dead-code elimination recovers little. candle with only the BERT and nomic modules would probably add a few MB, but this was not measured. The first implementation ticket should record the real stripped size.

### Latency (CPU)

The one like-for-like public benchmark is [jerrythomas/rust-embedding-bench](https://github.com/jerrythomas/rust-embedding-bench). It used all-MiniLM-L6-v2, an Apple M4 Max and 8 threads, with every backend validated at cosine ≥ 0.999 against sentence-transformers:

| Backend | Batch 1, short text, p50 | Batch 32, short text | Batch 32, long text |
|---|---|---|---|
| `ort` fp32 | 1.10 ms | 3,052 embeddings/s | 261 embeddings/s |
| `fastembed` | 1.71 ms | 2,849 embeddings/s | 315 embeddings/s |
| `candle` | 8.15 ms | 603 embeddings/s | 31 embeddings/s |

MiniLM-L6 is about the size of bge-small, so a query embedding should take a few milliseconds. nomic, at about 3× the parameters, and EmbeddingGemma, at about 9×, scale up roughly in proportion. This is a single third-party benchmark on Apple silicon, so the numbers will differ on x86 Linux. Cold start was not reported there and was not measured here. It is dominated by reading and initialising the model file: about 34 MB for int8 bge-small, compared with about 200 MB for q4 EmbeddingGemma. The prototype or implementation ticket should measure session creation for each candidate, since `serve` creates a session on every Agent connection. The session could be created lazily on the first search or write.

### Model delivery options

| Option | Verdict |
|---|---|
| **Bundle in the binary** (`include_bytes!`) | Possible only for int8 bge-small (about 34 MB), which would roughly double the binary. It would rule out choosing the model through `brain eval` (§13) without a rebuild, and it is impossible for EmbeddingGemma. Rejected. |
| **Download on first use** (fastembed's built-in `hf-hub`) | Works, but the cache goes to `.fastembed_cache` or `HF_HOME` and is not pinned to a revision. It also adds TLS and HTTP to the runtime binary and causes a surprise network wait during the first search. Rejected as the primary path. |
| **Fetched by `brain init`**, then verified and cached | Recommended. `init` is already interactive and needs the network. Pin `repo@revision` and SHA-256 for each file in the binary's model registry, and store the files in `$XDG_CACHE_HOME/second-brain/models/<model>@<rev>/`. Keep them outside the brain repo, which ADR-0004 makes a git repo and which must not carry hundreds of MB of weights. `serve` and `reindex` do the same fetch when the files are missing, for example on a second machine that cloned the brain without running `init`. That is a one-off foreground step, so it stays within ADR-0005. This needs an HTTP client in the binary. A small client such as `ureq` with `rustls` keeps it static. |

EmbeddingGemma's Gemma licence travels with the weights. The `onnx-community` mirror is not gated on Hugging Face, but `google/embeddinggemma-300m` is. If EmbeddingGemma is picked, `init` should state the licence.

## SPEC implications

1. **§6.2 step 3 (labels) is wrong as written.** `search_document:` and `passage:` are nomic and E5 conventions. bge-small uses a *query* instruction and no document label, and EmbeddingGemma uses templates. Each model should carry its own `query` and `document` templates in the provider, which may be empty. EmbeddingGemma's `title:` field could take the Recollection title.
2. **§6.1 metadata** should record the model ID **and revision or file hash**, not just the ID. Without that, re-quantising or switching ONNX variants would not trigger the rebuild.
3. **§12 `brain init`** should gain a step that fetches and verifies the embedding model. **§2 startup** should fetch the model when it is missing.
4. **§6.2 "no chunking"** collides with bge-small's 512-token limit, so long Recollections would be truncated silently. nomic (8192 tokens) and EmbeddingGemma (2048) are less affected. This is an input to the model-choice ticket.
5. **Platform support** needs a decision: Linux x86_64 and aarch64 (which covers WSL2) and macOS arm64 come for free. Intel Macs and pre-Haswell x86 machines do not.

## Newly surfaced questions

- Is Intel macOS a supported platform? If it is, the build must compile ONNX Runtime itself or ship `load-dynamic`.
- Should the model be loaded eagerly at `serve` start, or lazily on the first search or write? That depends on cold-start numbers, which are still unmeasured.
- How are the model weights distributed: straight from Hugging Face pinned by revision, or mirrored to the project's own release assets for availability?

## Sources

- pykeio/ort at `4fac49b` (2026-10-23): `ort-sys/build/main.rs`, `ort-sys/build/static_link/mod.rs`, `ort-sys/build/download/dist.tsv`, `docs/content/misc/prebuilt-binaries.mdx`, `docs/content/setup/linking.mdx` and `docs/content/setup/cargo-features.mdx`. The prebuilt archive (`sha256 02aac0d3…`) was downloaded and inspected.
- Anush008/fastembed-rs `main` (7.5.0): `Cargo.toml`, `README.md`, `src/models/text_embedding.rs`, `src/text_embedding/{init,impl,output}.rs`.
- huggingface/candle at `12bb5d1` (2026-10-14): `candle-transformers/src/models/`, `candle-examples/examples/{nomic-bert,bert_single_file_binary}`.
- microsoft/onnxruntime release v1.31.0, asset `onnxruntime-linux-x64-1.31.0.tgz`.
- Hugging Face model cards and the tree API for BAAI/bge-small-en-v1.5, Xenova/bge-small-en-v1.5, nomic-ai/nomic-embed-text-v1.5, google/embeddinggemma-300m and onnx-community/embeddinggemma-300m-ONNX.
- jerrythomas/rust-embedding-bench README, a third-party benchmark.
