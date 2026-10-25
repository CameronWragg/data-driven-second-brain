# SQLite with FTS5 and sqlite-vec in one static binary

Research for [#18](https://github.com/CameronWragg/data-driven-second-brain/issues/18). Checked 2026-10-25 against crate sources downloaded from crates.io, the sqlite-vec GitHub repo and the SQLite docs. No Rust toolchain was available, so nothing here was compiled. Every claim comes from reading source or docs.

## Answer

**Yes, with caveats.** `rusqlite` with `bundled` compiles SQLite with FTS5 turned on. The `sqlite-vec` crate compiles its C source with `SQLITE_CORE` and links it statically into the same binary. You register it in-process with `sqlite3_auto_extension`, so no loadable `.so`/`.dylib` is needed. Both libraries end up in one executable, and the binary has no runtime dependency on SQLite.

"One static binary" needs a qualifier. The SQLite stack is fully static on every target. Only Linux musl gives a binary with no dynamic libraries at all. On macOS, a fully static binary is not supported (see Trap 2), and musl builds of `sqlite-vec` 0.1.9 currently fail to compile (Trap 1).

### Recommended crate set

```toml
[dependencies]
rusqlite   = { version = "0.41", features = ["bundled"] }   # SQLite 3.54.1, FTS5 on
sqlite-vec = "0.1.9"                                        # last stable; see Trap 1 for musl
# Pass vectors as little-endian f32 bytes. zerocopy (IntoBytes) or bytemuck both work,
# and so does a hand-written f32::to_le_bytes loop.
```

Register sqlite-vec once, before the first `Connection::open`:

```rust
unsafe {
    rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
        sqlite_vec::sqlite3_vec_init as *const (),
    )));
}
```

(The newer safe-ish wrapper `rusqlite::auto_extension::register_auto_extension` takes the same function pointer.)

## Findings

### rusqlite `bundled`

- The latest versions are `rusqlite` 0.41.0 (2026-10-24) and `libsqlite3-sys` 0.39.1 (2026-10-25). Source: crates.io API.
- `bundled` turns on `libsqlite3-sys/bundled` and `modern_sqlite`. Source: `rusqlite-0.41.0/Cargo.toml` `[features]`.
- The bundled SQLite is **3.54.1**. Source: `libsqlite3-sys-0.39.1/sqlite3/sqlite3.h` (`SQLITE_VERSION`).
- The bundled build always passes `-DSQLITE_ENABLE_FTS5`, plus FTS3, JSON1, RTREE, STAT4, `SQLITE_THREADSAFE=1` and `SQLITE_ENABLE_LOAD_EXTENSION=1`. FTS5 needs no extra feature. Source: `libsqlite3-sys-0.39.1/build.rs` lines 168–186.
- rusqlite 0.41 exposes `auto_extension::{register_auto_extension, cancel_auto_extension, reset_auto_extension}`. Source: `rusqlite-0.41.0/src/auto_extension.rs`.
- The `load_extension` feature is **not** needed for static registration. Leave it off, because it only matters for loading `.so` files at runtime.

### How sqlite-vec is linked

- The `sqlite-vec` crate (0.1.9, by the upstream author) is a thin FFI wrapper. `build.rs` is one line: `cc::Build::new().file("sqlite-vec.c").define("SQLITE_CORE", None).compile("sqlite_vec0")`. `src/lib.rs` exports only `extern "C" fn sqlite3_vec_init()`. Source: `sqlite-vec-0.1.9/build.rs`, `src/lib.rs`.
- `SQLITE_CORE` makes the extension call `sqlite3_*` directly instead of through the loadable-extension API table. Those symbols resolve at link time to the SQLite that `libsqlite3-sys` compiled. The crate has **no** Cargo dependency on `libsqlite3-sys`. It relies on some other crate in the graph providing SQLite, which `rusqlite`/`bundled` does.
- The crate ships its own `sqlite3.h` (3.45.3) for compiling. That header is older than the linked library (3.54.1). This is harmless because the SQLite C ABI is backwards compatible, but it means sqlite-vec cannot use APIs added after 3.45.3.
- Upstream's Rust guide documents exactly this pattern (`bundled` plus `sqlite3_auto_extension`). Source: [`site/using/rust.md`](https://github.com/asg017/sqlite-vec/blob/main/site/using/rust.md).

### sqlite-vec maturity and limits

| Fact | Value | Source |
|---|---|---|
| Latest stable | 0.1.9 (2026-03-31); prereleases up to 0.1.10-alpha.4 (2026-05-18) | GitHub releases, crates.io |
| Stability | README: "pre-v1, so expect breaking changes" | README |
| Maintenance | Revived in March 2026 after a ~16-month gap (v0.1.6 Nov 2024 → v0.1.7 Mar 2026), sponsored by Mozilla; 8.2k stars, 244 open issues | v0.1.7 release notes, GitHub API |
| Search algorithm (stable) | **Brute force only**: `vec0` does exact KNN over chunked blobs | `sqlite-vec.c`; no ANN code in 0.1.9 |
| ANN | 0.1.10-alpha adds rescore and DiskANN (IVF present but not enabled), and the release notes say DiskANN deletes are "quite expensive" | v0.1.10-alpha.1/.2 notes |
| Max dimensions | 8192 per vector column | `SQLITE_VEC_VEC0_MAX_DIMENSIONS` |
| Max `k` | 4096 per KNN query | `SQLITE_VEC_VEC0_K_MAX` |
| Element types | `float[N]`, `int8[N]`, `bit[N]` | `sqlite-vec.c` column parser |
| Distance metrics | L2 (default), cosine, L1, set per column with `distance_metric=` | `Vec0DistanceMetrics` |
| Extra columns | up to 16 metadata (filterable in KNN `WHERE`), 4 partition keys, 16 auxiliary (`+col`) | `VEC0_MAX_*` constants; v0.1.6 notes |
| Chunk size | default ~1024 rows, max 4096 | `SQLITE_VEC_CHUNK_SIZE_MAX`; v0.1.7 notes |
| Deletes | Before 0.1.7, deletes only zeroed space. Since 0.1.7 a chunk is freed once *every* row in it is deleted. Partly empty chunks keep their space. 0.1.9 fixed a `DELETE` bug on long metadata text values | v0.1.7, v0.1.9 notes; `vec0Update_Delete_DeleteChunkIfEmpty` |
| Upserts | `INSERT OR REPLACE` only arrives in 0.1.10-alpha.3. On 0.1.9, update by `DELETE` then `INSERT` | v0.1.10-alpha.3 notes |
| SIMD | The Rust crate's `build.rs` defines neither `SQLITE_VEC_ENABLE_AVX` nor `_NEON`, so distance kernels are scalar. Upstream's own Makefile turns SIMD on only for macOS | crate `build.rs`; upstream `Makefile` lines 38–45 |

**Fit for the Second Brain:** candidate models have 384 dimensions (bge-small) or 768 (nomic, EmbeddingGemma), well under 8192. One embedding per Recollection or Entity means thousands to low tens of thousands of rows. My estimate is that scalar brute force over 10k × 768 floats takes single-digit milliseconds. That is an estimate, not a measurement, and should be benchmarked. ANN is not needed for v1, so stay on the stable flat index.

### WAL with several processes (SPEC §6.1, §11)

From [sqlite.org/wal.html](https://www.sqlite.org/wal.html), [lang_transaction.html](https://www.sqlite.org/lang_transaction.html) and [howtocorrupt.html](https://www.sqlite.org/howtocorrupt.html):

- Readers do not block writers and writers do not block readers, but only one writer runs at a time. This matches §11: one lock-holding writer and many `serve` readers.
- **All processes must be on the same host.** WAL does not work on a network filesystem because the `-shm` wal-index is shared memory. A brain folder on NFS/SMB, or one live-synced by Dropbox and similar tools, is unsafe.
- WAL databases cannot be opened read-only without write access to `-shm` or its directory.
- **`SQLITE_BUSY` still happens in WAL** in a few cases: while the last connection closes and cleans up WAL/shm, during crash recovery by the first connection to open after a crash, and when another connection uses exclusive locking mode. Set a `busy_timeout` on every connection, including readers.
- **A deferred transaction that reads and then writes can fail with `SQLITE_BUSY` without retrying**, if another writer committed in between, because the snapshot cannot be upgraded. Start write transactions with `BEGIN IMMEDIATE` (`TransactionBehavior::Immediate` in rusqlite), even under the app-level `.index/lock`.
- **Checkpoint starvation:** the WAL only resets when no reader holds an old snapshot. Long-lived read transactions in idle `serve` processes would let `-wal` grow without bound. Keep read transactions short and never hold a statement or transaction open across MCP calls.
- Automatic checkpoints run at 1000 WAL pages by default. Leave this on.

## Traps

1. **musl builds of sqlite-vec 0.1.9 fail to compile.** Lines 68–70 of `sqlite-vec.c` say `typedef u_int8_t uint8_t;` (and the 16- and 64-bit versions). musl does not define BSD `u_int*_t`. This is [issue #314](https://github.com/asg017/sqlite-vec/issues/314), open, and a duplicate of the earlier #156. The fix (commit `85cf415`) first shipped in **0.1.10-alpha.2**, so no stable release has it. For a fully static Linux binary, the options are:
   - pin `sqlite-vec = "=0.1.10-alpha.4"`, which also brings in the untested ANN code;
   - vendor or `[patch]` 0.1.9 with the three lines removed;
   - (untested) set `CFLAGS_<target>=-Du_int8_t=uint8_t -Du_int16_t=uint16_t -Du_int64_t=uint64_t`.
2. **macOS cannot have a fully static binary.** Apple: "Apple does not support statically linked binaries on Mac OS X" ([QA1118](https://developer.apple.com/library/archive/qa/qa1118/_index.html)). A macOS `brain` always links `libSystem` dynamically. This is fine, because libSystem is always present. On Linux, Rust links glibc dynamically by default. Only the `*-linux-musl` targets are static by default ([Rust reference, linkage](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes)). "Dependency-free" (ADR-0003) is achievable as "only depends on the OS's libc" without musl.
3. **Never link two copies of SQLite into one process.** SQLite's POSIX-lock workaround keeps a global list of open files, and two copies corrupt each other's locking (howtocorrupt §2.3). Make sure no other crate pulls in a system `libsqlite3` or a second bundled copy, for example `sqlx` with its own `libsqlite3-sys` version. Two different `libsqlite3-sys` versions fail at Cargo resolution because of `links = "sqlite3"`, which is good. Watch for crates that vendor SQLite under a different `links` name.
4. **`brain reindex` must not delete or replace the database file while other `serve` processes have it open.** Unlinking or renaming an open database and creating a new one with the same name makes processes share a WAL across two different files, which can corrupt it (howtocorrupt §2.5). Rebuild *inside* the existing file under the write lock: drop and recreate the tables in one `BEGIN IMMEDIATE` transaction. Alternatively, use a generation-named file that readers switch to.
5. **Register the auto-extension before opening any connection.** It applies only to connections opened after registration. The auto-extension must not open a database itself, or it recurses (rusqlite safety docs).
6. **KNN plus filters.** A `vec0` KNN query needs `MATCH` and either `k = ?` or `LIMIT`, but not both. Only declared metadata or partition-key columns filter *inside* the KNN. A `WHERE` on anything joined afterwards filters the top-k, so you can get fewer than k results. Scope rules in SPEC §6.4 (Repository, Branch, Kind) should be metadata columns on the `vec0` table, or you over-fetch `k` and then filter.
7. **The upstream Rust example is stale.** It uses `zerocopy::AsBytes`, which was renamed `IntoBytes` in zerocopy 0.8. The docs site is also described as stale in the v0.1.7 notes.
8. **No SIMD in the crate build.** Performance is scalar. This matters only if the brain grows to hundreds of thousands of vectors.

## Sources

- crates.io: `rusqlite` 0.41.0, `libsqlite3-sys` 0.39.1, `sqlite-vec` 0.1.9 (sources downloaded and read: `Cargo.toml`, `build.rs`, `src/auto_extension.rs`, `sqlite-vec.c`, `sqlite3/sqlite3.h`)
- https://github.com/asg017/sqlite-vec: README, releases v0.1.4 to v0.1.10-alpha.4, `site/using/rust.md`, `Makefile` at v0.1.9, issues #156 and #314
- https://www.sqlite.org/wal.html, https://www.sqlite.org/lang_transaction.html, https://www.sqlite.org/howtocorrupt.html
- https://doc.rust-lang.org/reference/linkage.html
- https://developer.apple.com/library/archive/qa/qa1118/_index.html
