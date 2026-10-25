# PROTOTYPE findings: embeddings and sqlite-vec in one binary

Throwaway spike for wayfinder ticket [Spike: embeddings and sqlite-vec in one binary](https://github.com/CameronWragg/data-driven-second-brain/issues/23). This branch is never merged. Measured on 2026-10-25.

**Machine:** Intel i5-1245U laptop (2P+8E cores, 12 threads), 32 GB RAM, Arch Linux (glibc), rustc 1.99.1. The OS page cache was warm: the model files had already been read once.

**Stack:** `fastembed` 7.5.0 (`default-features = false`, `ort-download-binaries-rustls-tls`) on `ort` 2.0.0-rc.15 with ONNX Runtime statically linked; `rusqlite` 0.41 `bundled` (SQLite 3.54.1); `sqlite-vec` 0.1.9 registered through `sqlite3_auto_extension`. Models were loaded with `try_new_from_user_defined` from local files.

**To run:** `cargo build --release`, put the model files under `~/.cache/second-brain-spike/models/<dir>/`, then `spike-embed-vec embed|hybrid <dir> [n]`, `writers` or `write-proc <n>`.

## Verdict

**Yes.** The whole stack builds and runs as one binary on glibc Linux with no build-time surprises. Hybrid FTS5 + vec0 + RRF search works and is fast enough. Two things the research missed matter for the backlog: fastembed won't batch dynamically quantised models, and the int8 bge-small that fastembed itself uses is the slower of the two.

## Binary

| | |
|---|---|
| Stripped release size (fat LTO, `codegen-units = 1`) | **30.6 MB** for everything: ONNX Runtime, tokenizers, SQLite, sqlite-vec |
| Dynamic dependencies | `libc`, `libm`, `libstdc++`, `libgcc_s`, nothing else (no OpenSSL) |
| Clean release build | 2 min 43 s (9.5 min CPU). Incremental release rebuild: about 33 s |

## Embedding (one process, page cache warm)

| Model (file) | Weights | Session create | Start → first embedding | Query p50 / p95 | 347-word doc | Reindex, short docs | Peak RSS |
|---|---|---|---|---|---|---|---|
| bge-small int8, `Xenova/bge-small-en-v1.5` `onnx/model_quantized.onnx` (dynamic quant) | 34 MB | 241 ms | **250 ms** | **11.9 / 14.4 ms** | 166 ms | 71/s (no batching) | **147 MB** |
| bge-small int8, `Qdrant/bge-small-en-v1.5-onnx-Q` `model_optimized.onnx` (static quant, fastembed's `BGESmallENV15Q`) | 66 MB | 230 ms | 328 ms | 35.6 / 41.3 ms | 289 ms | 65/s (batch 32) | 336 MB |
| bge-small fp32, `Xenova/bge-small-en-v1.5` `onnx/model.onnx` | 133 MB | 459 ms | 475 ms | 15.6 / 18.7 ms | 242 ms | 85/s (batch 32) | 346 MB |
| nomic v1.5 int8, `nomic-ai/nomic-embed-text-v1.5` `onnx/model_quantized.onnx` (dynamic quant) | 137 MB | 504 ms | **535 ms** | **24.6 / 29.7 ms** | 519 ms | 23/s (no batching) | **407 MB** |

Revisions used: Xenova `ea104dac`, Qdrant `aa8f8b06`, nomic `e9b67630`.

- **Dynamic-quant models cannot be batched.** fastembed returns `InvalidArgument("Dynamic quantization cannot be used with batching…")` for any explicit batch size, because the quantisation range would vary from batch to batch. Xenova bge-small int8 and nomic int8 are both dynamic. The provider has to embed one text per call, which caps reindex throughput.
- **The Qdrant "Q" bge-small is worse than Xenova's int8 on every measure:** twice the size, about 3× slower per query, and over twice the RSS. The research's 34 MB figure was Xenova's file.
- **Threads:** `intra_threads` of 2 or 4 matched or beat "all cores" for single queries (7–8 ms against 12 ms for bge int8) on this P/E-core CPU. Long documents favoured more threads. The default (all cores) is acceptable, but a cap of about 4 costs little and is kinder to a laptop.
- **Reindex cost:** at roughly 70 short docs/s (bge int8) or 23/s (nomic int8), 1,000 Recollections take about 15–45 s. This is a visible foreground wait for `brain reindex` and on a model change, so it needs a progress indicator.
- **Memory multiplies with sessions:** every `serve` process holds its own model, about 150 MB for bge int8 or 400 MB for nomic int8. Five concurrent Agents on nomic is about 2 GB.

## Hybrid search (FTS5 + vec0, RRF k=60, top-k 10)

The 10 real Recollection-shaped texts were embedded for real. The remaining rows are synthetic text plus random unit vectors, so only the latency and filtering numbers are meaningful at scale. The scope filter is `repo` as a vec0 `partition key`, with `branch` and `kind` as metadata columns.

| Rows | Model | Bulk insert (rec + fts + vec, one tx) | DB + WAL on disk | Embed p50 | FTS5 p50 | vec KNN p50 / p95 | Total query p50 / p95 |
|---|---|---|---|---|---|---|---|
| 10k | bge int8 (384-d) | 1.3 s | 42 MB | 8.6 ms | 0.28 ms | 4.7 / 13.5 ms | **17.7 / 26.3 ms** |
| 10k | nomic int8 (768-d) | 3.1 s | 77 MB | 22.5 ms | 0.34 ms | 7.6 / 26.6 ms | **39.9 / 51.1 ms** |
| 50k | bge int8 | 7.0 s | 193 MB | 7.7 ms | 0.60 ms | 14.2 / 52.5 ms | 25.4 / 61.7 ms |
| 50k | nomic int8 | 15.6 s | 350 MB | — | — | 26.7 / 105 ms | 53.0 / 128.6 ms |

The on-disk size includes an uncheckpointed WAL from the single bulk transaction, so it overstates the steady state.

- **Relevance:** RRF put the right Recollection first for all four test queries with both models. One query ("verify payment webhook signatures") got **zero FTS5 hits** and was rescued entirely by the vector side, which shows that hybrid search earns its keep.
- **Filters run inside the KNN:** a `(repo, branch, kind)` bucket of 57 rows queried with k=200 returned exactly 57 rows. Partition and metadata columns avoid the post-filter shortfall.
- **Updating a vector** (delete then insert in one `BEGIN IMMEDIATE` transaction) took 5 ms at 10k rows and 70–80 ms at 50k rows or 768-d.
- **Brute-force KNN is fine up to about 10k rows,** with p95 under 30 ms. At 50k rows the p95 on 768-d passes 100 ms. Real brains are likely to stay in the low thousands, so ANN (sqlite-vec 0.1.10-alpha) is not needed for v1.

## Concurrency

- 2 writer threads and 1 reader thread, each on its own connection: 1,000 writes and **0 `SQLITE_BUSY`**.
- **4 separate processes** each doing 500 `BEGIN IMMEDIATE` single-row writes on the same WAL file, with `busy_timeout = 5 s`: 2,000 writes and **0 `SQLITE_BUSY`**. Each process finished in 23–92 ms.

## Implications for the backlog and SPEC

1. **Model load should be lazy or backgrounded, not block the MCP handshake.** Start to first embedding takes 250 ms (bge int8) to 535 ms (nomic int8) with a warm cache, and more with a cold one. Claude Code expects a fast `initialize`. Answering it immediately and loading the session on a thread (inside `serve`, so ADR-0005 holds) hides most of the delay. The first search waits on the load.
2. **The embedding provider must embed one text per call** when the model is dynamically quantised. Carry `batchable` (or the quantisation mode) per model in the model registry.
3. **Pin the Xenova int8 file for bge-small,** not fastembed's Qdrant one. This feeds the default-model fog: bge int8 is about 2.7× cheaper in RSS and 2× faster than nomic int8 here. Quality still needs eval data.
4. **Performance budgets** (fog) now have a baseline: a 30 MB binary, about 250–550 ms to a ready model, a hybrid query p95 of about 26–51 ms at 10k rows, and 150–400 MB RSS per `serve`.
5. **Per-`serve` memory** is an input to [Decide the sync and shutdown model](https://github.com/CameronWragg/data-driven-second-brain/issues/31), since every Agent connection spawns its own process with its own model.
6. **Dev environment:** a clean fat-LTO release build takes about 3 minutes. Day-to-day builds should use the dev profile or thin LTO.

## Addendum: first-party files and batching

Asked after resolution: can we avoid third-party conversions, and get batching?

**First-party ONNX availability (Hugging Face, 2026-10-25):**
- `BAAI/bge-small-en-v1.5` publishes **only fp32**, as `onnx/model.onnx` (133 MB, revision `5c38ec7c`, LFS `828e1496…`). It is byte-identical to Xenova's fp32 file. There is no first-party int8.
- `nomic-ai/nomic-embed-text-v1.5` hosts fp32 (547 MB), fp16, int8/quantized (137 MB), uint8, q4, q4f16 and bnb4 in its own repo. They were contributed by Xenova and merged by Nomic, in PRs #3 and #39.
- `google/embeddinggemma-300m` has **no ONNX** (and is gated). The only ONNX is `onnx-community/embeddinggemma-300m-ONNX`, which is Hugging Face's org, not Google's.

**Realistic reindex** (`batch` subcommand): 128 Recollections of 20–300 words, mean 60 words. "Sorted" means batched by length to cut padding.

| Model | Single | Batch 8 unsorted | **Batch 8 sorted** | Batch 32 sorted | cos(batched, single) | cos(vs fp32) |
|---|---|---|---|---|---|---|
| bge fp32 (BAAI) | 20.1/s | 17.1/s | **25.2/s** | 22.3/s | 1.00000 (bit-stable) | — |
| bge int8 (Xenova), guard bypassed | 32.4/s | 30.0/s | 45.7/s | 37.5/s | min 0.9937, mean 0.9965 | mean 0.9964, min 0.9936 |
| nomic fp32 (Nomic) | 5.2/s | 3.5/s | 5.2/s | 4.6/s | 1.00000 | — |
| nomic int8 (Nomic), guard bypassed | 9.5/s | 6.8/s | 10.4/s | 8.8/s | min 0.967, mean 0.983 | mean 0.959, min 0.947 |

- **Batching gains little on this CPU.** ONNX Runtime already spreads a single item across the intra-op threads. Length-sorted batch 8 is the best setting, at +25% for bge fp32 and +40% for bge int8. **Unsorted batching is slower than embedding one text at a time**, because of padding. Batch 32 is worse than batch 8.
- **fastembed's guard is justified.** With dynamic quantisation, a batched vector differs from the single vector of the same text (cos about 0.996 for bge, 0.98 for nomic). That is as large as the whole int8-to-fp32 gap, so whether two Recollections look like duplicates would depend on which batch they were embedded in. Dynamic-quant models must stay one at a time.
- **nomic int8 is lossy:** cos 0.959 to its own fp32. bge int8 loses much less (0.996).
- **nomic fp32 is heavy:** 1.19 s from start to first embedding, a 54 ms query p50 and **1.43 GB peak RSS**.
- **bge fp32 from BAAI** gives deterministic, batchable vectors from first-party weights at 133 MB, a 16 ms query, 346 MB RSS and about 25 docs/s for 60-word docs (roughly 40 s per 1,000 Recollections).
