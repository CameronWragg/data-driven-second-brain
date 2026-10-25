//! PROTOTYPE — throwaway spike for wayfinder ticket #23
//! ("Spike: embeddings and sqlite-vec in one binary"). Never merged; only the
//! findings feed the backlog.
//!
//! Usage:
//!   spike-embed-vec embed  <model-dir>            # cold start + latency
//!   spike-embed-vec hybrid <model-dir> [n]        # FTS5 + vec0 + RRF at n rows
//!   spike-embed-vec writers                       # BEGIN IMMEDIATE contention
//!
//! <model-dir> holds the .onnx file plus the four tokenizer files.

use std::{fs, path::Path, time::Instant};

use fastembed::{
    InitOptionsUserDefined, Pooling, QuantizationMode, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
use rusqlite::{Connection, ffi, params};

const DB: &str = "PROTOTYPE-wipe-me.db";

/// Recollection-shaped texts: title + body, like the SPEC's Kinds.
const RECOLLECTIONS: &[(&str, &str, &str, &str)] = &[
    ("acme/api", "main", "gotcha", "Postgres advisory locks leak across pgbouncer transaction pooling. Use session pooling for the migrator or switch to a lock table."),
    ("acme/api", "main", "convention", "Handlers return Result<Response, ApiError>; ApiError maps to RFC 7807 problem+json. Never return a bare 500."),
    ("acme/api", "feat/billing", "decision", "Stripe webhooks are verified with the raw request body before JSON parsing, because the signature covers bytes not values."),
    ("acme/web", "main", "gotcha", "Next.js middleware runs on the edge runtime, so node:crypto is unavailable; use the Web Crypto API for HMAC."),
    ("acme/web", "main", "how-to", "To regenerate the GraphQL client run pnpm codegen after pulling schema.graphql from the api repo."),
    ("acme/infra", "main", "decision", "Terraform state lives in S3 with DynamoDB locking, one state file per environment, never per module."),
    ("acme/infra", "main", "gotcha", "The EKS node group AMI auto-update restarts nodes during business hours unless the maintenance window tag is set."),
    ("acme/api", "main", "how-to", "Run integration tests with docker compose up -d db then cargo test --features it; the db port is 55432 to avoid clashes."),
    ("acme/web", "main", "convention", "Components live next to their tests; snapshot tests are banned, use testing-library queries by role."),
    ("acme/api", "main", "gotcha", "Serde untagged enums swallow the real deserialisation error; prefer an internally tagged enum for API payloads."),
];

const QUERIES: &[(&str, Option<&str>)] = &[
    ("why does the database lock break with connection pooling", Some("acme/api")),
    ("how do I verify payment webhook signatures", None),
    ("crypto not available in middleware", Some("acme/web")),
    ("where is terraform state stored", None),
];

fn main() {
    let t0 = Instant::now();
    unsafe {
        // sqlite-vec is compiled in with SQLITE_CORE; register it for every
        // connection opened after this point.
        ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(*mut ffi::sqlite3, *mut *mut i8, *const ffi::sqlite3_api_routines) -> i32,
        >(sqlite_vec::sqlite3_vec_init as *const ())));
    }
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("embed") => embed(Path::new(&args[2]), t0),
        Some("hybrid") => hybrid(
            Path::new(&args[2]),
            args.get(3).map(|s| s.parse().unwrap()).unwrap_or(10_000),
        ),
        Some("writers") => writers(),
        Some("batch") => batch(Path::new(&args[2]), args.get(3).map(Path::new)),
        Some("write-proc") => write_proc(args[2].parse().unwrap()),
        _ => eprintln!("usage: embed <dir> | hybrid <dir> [n] | writers"),
    }
}

struct Model {
    te: TextEmbedding,
    query_prefix: &'static str,
    doc_prefix: &'static str,
    dim: usize,
    batchable: bool,
}

fn load(dir: &Path) -> Model {
    let onnx = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "onnx"))
        .unwrap();
    let read = |f: &str| fs::read(dir.join(f)).unwrap();
    let tok = TokenizerFiles {
        tokenizer_file: read("tokenizer.json"),
        config_file: read("config.json"),
        special_tokens_map_file: read("special_tokens_map.json"),
        tokenizer_config_file: read("tokenizer_config.json"),
    };
    let nomic = dir.to_string_lossy().contains("nomic");
    let (pooling, quant, qp, dp, dim) = if nomic {
        (Pooling::Mean, if dir.to_string_lossy().contains("fp32") { QuantizationMode::None } else { QuantizationMode::Dynamic }, "search_query: ", "search_document: ", 768)
    } else {
        let quant = if dir.to_string_lossy().contains("fp32") {
            QuantizationMode::None
        } else if dir.to_string_lossy().contains("xenova") {
            QuantizationMode::Dynamic
        } else {
            QuantizationMode::Static
        };
        (Pooling::Cls, quant, "Represent this sentence for searching relevant passages: ", "", 384)
    };
    // FORCE_BATCH=1 lies to fastembed about dynamic quant to bypass its guard,
    // so the batch-vs-single drift can be measured.
    let declared = if std::env::var("FORCE_BATCH").is_ok() { QuantizationMode::None } else { quant };
    let model = UserDefinedEmbeddingModel::new(fs::read(onnx).unwrap(), tok)
        .with_pooling(pooling)
        .with_quantization(declared);
    let mut opts = InitOptionsUserDefined::default();
    opts.intra_threads = std::env::var("THREADS").ok().map(|t| t.parse().unwrap());
    let te = TextEmbedding::try_new_from_user_defined(model, opts).unwrap();
    Model { te, query_prefix: qp, doc_prefix: dp, dim, batchable: quant != QuantizationMode::Dynamic }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn pct(mut v: Vec<f64>, p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p) as usize]
}

fn embed(dir: &Path, t0: Instant) {
    let t = Instant::now();
    let mut m = load(dir);
    let load_ms = ms(t);
    let t = Instant::now();
    let first = m.te.embed(vec![format!("{}{}", m.query_prefix, QUERIES[0].0)], None).unwrap();
    let first_ms = ms(t);
    println!("model load (session create): {load_ms:.0} ms");
    println!("first query embedding:       {first_ms:.1} ms");
    println!("process start -> first emb:  {:.0} ms (dim {})", ms(t0), first[0].len());

    let mut q = vec![];
    for i in 0..50 {
        let t = Instant::now();
        m.te.embed(vec![format!("{}{}", m.query_prefix, QUERIES[i % QUERIES.len()].0)], None)
            .unwrap();
        q.push(ms(t));
    }
    println!("single query embed  p50 {:.1} ms  p95 {:.1} ms", pct(q.clone(), 0.5), pct(q, 0.95));

    // A long Recollection: ~450 words, close to bge-small's 512-token cap.
    let long = RECOLLECTIONS.iter().map(|r| r.3).collect::<Vec<_>>().join(" ").repeat(2);
    let mut d = vec![];
    for _ in 0..10 {
        let t = Instant::now();
        m.te.embed(vec![format!("{}{long}", m.doc_prefix)], None).unwrap();
        d.push(ms(t));
    }
    println!("single long doc embed p50 {:.1} ms ({} words)", pct(d, 0.5), long.split_whitespace().count());

    let docs: Vec<String> = (0..256)
        .map(|i| format!("{}{}", m.doc_prefix, RECOLLECTIONS[i % RECOLLECTIONS.len()].3))
        .collect();
    // Dynamic-quant models refuse batch sizes, so reindex one text at a time.
    let t = Instant::now();
    if m.batchable {
        m.te.embed(&docs, Some(32)).unwrap();
    } else {
        for d in &docs {
            m.te.embed(vec![d], None).unwrap();
        }
    }
    println!("reindex (batch={}): {:.0} short docs/s", m.batchable, 256.0 / t.elapsed().as_secs_f64());
    let status = fs::read_to_string("/proc/self/status").unwrap();
    println!("{}", status.lines().find(|l| l.starts_with("VmHWM")).unwrap());
}

fn vec_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self, dim: usize) -> Vec<f32> {
        let v: Vec<f32> = (0..dim).map(|_| (self.next() % 2000) as f32 / 1000.0 - 1.0).collect();
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / n).collect()
    }
}

fn open() -> Connection {
    let c = Connection::open(DB).unwrap();
    c.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
    c.pragma_update(None, "synchronous", "NORMAL").unwrap();
    c
}

fn hybrid(dir: &Path, n: usize) {
    let _ = fs::remove_file(DB);
    let _ = fs::remove_file(format!("{DB}-wal"));
    let _ = fs::remove_file(format!("{DB}-shm"));
    let mut m = load(dir);
    let c = open();
    let (sqlite, vec): (String, String) = c
        .query_row("select sqlite_version(), vec_version()", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    println!("sqlite {sqlite}, sqlite-vec {vec}");
    c.execute_batch(&format!(
        "create table rec(id integer primary key, repo text, branch text, kind text, body text);
         create virtual table rec_fts using fts5(body, content='rec', content_rowid='id');
         create virtual table rec_vec using vec0(
           embedding float[{}] distance_metric=cosine,
           repo text partition key, branch text, kind text);",
        m.dim
    ))
    .unwrap();

    // Real embeddings for the 10 Recollections, filler for the rest.
    let docs: Vec<String> =
        RECOLLECTIONS.iter().map(|r| format!("{}{}", m.doc_prefix, r.3)).collect();
    let embs: Vec<Vec<f32>> = docs.iter().map(|d| m.te.embed(vec![d], None).unwrap().remove(0)).collect();
    let words = ["cache", "retry", "timeout", "schema", "deploy", "queue", "token", "index",
        "build", "flaky", "lint", "auth", "cron", "proxy", "pool", "shard"];
    let repos = ["acme/api", "acme/web", "acme/infra", "acme/mobile", "acme/data"];
    let kinds = ["gotcha", "convention", "decision", "how-to"];
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let t = Instant::now();
    c.execute_batch("begin immediate").unwrap();
    {
        let mut ins = c.prepare("insert into rec(id,repo,branch,kind,body) values(?,?,?,?,?)").unwrap();
        let mut fts = c.prepare("insert into rec_fts(rowid,body) values(?,?)").unwrap();
        let mut vins = c.prepare("insert into rec_vec(rowid,embedding,repo,branch,kind) values(?,?,?,?,?)").unwrap();
        for i in 0..n {
            let id = i as i64 + 1;
            let (repo, branch, kind, body, emb) = if i < RECOLLECTIONS.len() {
                let r = RECOLLECTIONS[i];
                (r.0.to_string(), r.1.to_string(), r.2.to_string(), r.3.to_string(), embs[i].clone())
            } else {
                let body = (0..30).map(|_| words[(rng.next() % 16) as usize]).collect::<Vec<_>>().join(" ");
                (
                    repos[(rng.next() % 5) as usize].to_string(),
                    if rng.next() % 10 == 0 { "feat/x".into() } else { "main".into() },
                    kinds[(rng.next() % 4) as usize].to_string(),
                    body,
                    rng.unit(m.dim),
                )
            };
            ins.execute(params![id, repo, branch, kind, body]).unwrap();
            fts.execute(params![id, body]).unwrap();
            vins.execute(params![id, vec_bytes(&emb), repo, branch, kind]).unwrap();
        }
    }
    c.execute_batch("commit").unwrap();
    println!("insert {n} rows (rec + fts + vec): {:.0} ms", ms(t));
    let size = fs::metadata(DB).unwrap().len() + fs::metadata(format!("{DB}-wal")).map(|m| m.len()).unwrap_or(0);
    println!("db + wal size: {:.1} MB", size as f64 / 1e6);

    let k = 10;
    let mut lat_embed = vec![];
    let mut lat_fts = vec![];
    let mut lat_vec = vec![];
    let mut lat_all = vec![];
    for round in 0..20 {
        for (q, repo) in QUERIES {
            let ta = Instant::now();
            let t = Instant::now();
            let qe = m.te.embed(vec![format!("{}{q}", m.query_prefix)], None).unwrap().remove(0);
            lat_embed.push(ms(t));

            // FTS5: OR the query terms so natural-language queries still match.
            let fts_q = q.split_whitespace().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(" OR ");
            let t = Instant::now();
            let fts_ids: Vec<i64> = match repo {
                Some(r) => c.prepare_cached(
                    "select rec_fts.rowid from rec_fts join rec on rec.id = rec_fts.rowid
                     where rec_fts match ? and rec.repo = ? order by bm25(rec_fts) limit ?").unwrap()
                    .query_map(params![fts_q, r, k], |r| r.get(0)).unwrap().map(Result::unwrap).collect(),
                None => c.prepare_cached(
                    "select rowid from rec_fts where rec_fts match ? order by bm25(rec_fts) limit ?").unwrap()
                    .query_map(params![fts_q, k], |r| r.get(0)).unwrap().map(Result::unwrap).collect(),
            };
            lat_fts.push(ms(t));

            let t = Instant::now();
            let vec_rows: Vec<(i64, f64)> = match repo {
                Some(r) => c.prepare_cached(
                    "select rowid, distance from rec_vec where embedding match ? and k = ?
                     and repo = ? and branch = 'main'").unwrap()
                    .query_map(params![vec_bytes(&qe), k, r], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
                    .map(Result::unwrap).collect(),
                None => c.prepare_cached(
                    "select rowid, distance from rec_vec where embedding match ? and k = ?").unwrap()
                    .query_map(params![vec_bytes(&qe), k], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
                    .map(Result::unwrap).collect(),
            };
            lat_vec.push(ms(t));

            // Reciprocal rank fusion, k = 60.
            let mut scores: std::collections::HashMap<i64, f64> = Default::default();
            for (rank, id) in fts_ids.iter().enumerate() {
                *scores.entry(*id).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
            }
            for (rank, (id, _)) in vec_rows.iter().enumerate() {
                *scores.entry(*id).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
            }
            let mut fused: Vec<_> = scores.into_iter().collect();
            fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            lat_all.push(ms(ta));

            if round == 0 {
                let top = fused[0].0;
                let (trepo, tbody): (String, String) = c
                    .query_row("select repo, body from rec where id = ?", [top], |r| Ok((r.get(0)?, r.get(1)?)))
                    .unwrap();
                println!(
                    "\nQ: {q}  [scope {}]\n   fts hits {}, vec hits {} (k={k}), nearest dist {:.3}\n   top: [{trepo}] {}",
                    repo.unwrap_or("all"), fts_ids.len(), vec_rows.len(),
                    vec_rows.first().map(|v| v.1).unwrap_or(f64::NAN),
                    &tbody[..tbody.len().min(90)]
                );
            }
        }
    }
    println!("\nper-query latency at {n} rows (p50 / p95 ms):");
    for (name, v) in [("embed", lat_embed), ("fts5", lat_fts), ("vec knn", lat_vec), ("total", lat_all)] {
        println!("  {name:8} {:6.2} / {:6.2}", pct(v.clone(), 0.5), pct(v, 0.95));
    }

    // Selective filter: a (repo, kind, branch) bucket with few rows must still
    // return min(k, bucket size) because the filter runs inside the KNN.
    let bucket: i64 = c
        .query_row("select count(*) from rec where repo='acme/mobile' and kind='gotcha' and branch='feat/x'", [], |r| r.get(0))
        .unwrap();
    let got: i64 = c
        .query_row(
            "select count(*) from (select rowid from rec_vec where embedding match ? and k = 200
             and repo = 'acme/mobile' and kind = 'gotcha' and branch = 'feat/x')",
            [vec_bytes(&rng.unit(m.dim))],
            |r| r.get(0),
        )
        .unwrap();
    println!("\nselective filter: bucket has {bucket} rows, knn k=200 returned {got} (want {})", bucket.min(200));

    // Update path: vec0 0.1.9 has no INSERT OR REPLACE, so delete then insert.
    let t = Instant::now();
    c.execute_batch("begin immediate").unwrap();
    c.execute("delete from rec_vec where rowid = 1", []).unwrap();
    c.execute(
        "insert into rec_vec(rowid,embedding,repo,branch,kind) values(1,?,'acme/api','main','gotcha')",
        [vec_bytes(&embs[0])],
    )
    .unwrap();
    c.execute_batch("commit").unwrap();
    println!("vec0 update (delete+insert in one tx): {:.2} ms", ms(t));
}

/// Two connections hammering BEGIN IMMEDIATE writes while a third reads:
/// with busy_timeout set, nobody should see SQLITE_BUSY.
fn writers() {
    let _ = fs::remove_file(DB);
    open().execute_batch("create table t(id integer primary key, w int, v text)").unwrap();
    let handles: Vec<_> = (0..3)
        .map(|w| {
            std::thread::spawn(move || {
                let c = open();
                let mut busy = 0;
                for i in 0..500 {
                    if w == 2 {
                        let _: i64 = c.query_row("select count(*) from t", [], |r| r.get(0)).unwrap();
                        continue;
                    }
                    match c.execute_batch(&format!(
                        "begin immediate; insert into t(w,v) values({w}, 'x{i}'); commit;"
                    )) {
                        Ok(()) => {}
                        Err(e) => {
                            busy += 1;
                            eprintln!("writer {w}: {e}");
                            let _ = c.execute_batch("rollback");
                        }
                    }
                }
                busy
            })
        })
        .collect();
    let busy: i32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let rows: i64 = open().query_row("select count(*) from t", [], |r| r.get(0)).unwrap();
    println!("concurrent writers: {rows} rows written, {busy} busy errors");
}

/// One of several separate processes writing to the same file, as concurrent
/// `serve` processes would.
fn write_proc(w: i64) {
    let c = open();
    c.execute_batch("create table if not exists p(id integer primary key, w int, v text)").unwrap();
    let mut busy = 0;
    let t = Instant::now();
    for i in 0..500 {
        if let Err(e) = c.execute_batch(&format!(
            "begin immediate; insert into p(w,v) values({w}, 'x{i}'); commit;"
        )) {
            busy += 1;
            eprintln!("proc {w}: {e}");
            let _ = c.execute_batch("rollback");
        }
    }
    println!("proc {w}: 500 writes in {:.0} ms, {busy} busy errors", ms(t));
}

fn cos(a: &[f32], b: &[f32]) -> f32 {
    let d: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    d / (a.iter().map(|x| x * x).sum::<f32>().sqrt() * b.iter().map(|x| x * x).sum::<f32>().sqrt())
}

/// Realistic reindex: 128 Recollections of 20–300 words. Compares one-at-a-time
/// against batching (unsorted and length-sorted), reports throughput and the
/// cosine drift between batched and single vectors. With a reference model
/// dir, also reports agreement with that model's single vectors.
fn batch(dir: &Path, reference: Option<&Path>) {
    let mut m = load(dir);
    let mut rng = Rng(42);
    let docs: Vec<String> = (0..128)
        .map(|_| {
            let n = 1 + (rng.next() % 6) as usize;
            let body = (0..n)
                .map(|_| RECOLLECTIONS[(rng.next() % RECOLLECTIONS.len() as u64) as usize].3)
                .collect::<Vec<_>>()
                .join(" ");
            format!("{}{body}", m.doc_prefix)
        })
        .collect();
    let words: usize = docs.iter().map(|d| d.split_whitespace().count()).sum();
    println!("{} docs, mean {} words", docs.len(), words / docs.len());

    let t = Instant::now();
    let single: Vec<Vec<f32>> = docs.iter().map(|d| m.te.embed(vec![d], None).unwrap().remove(0)).collect();
    println!("  single           {:6.1} docs/s", 128.0 / t.elapsed().as_secs_f64());

    let mut order: Vec<usize> = (0..docs.len()).collect();
    order.sort_by_key(|&i| docs[i].len());
    for bs in [8usize, 32] {
        for sorted in [false, true] {
            let idx: Vec<usize> = if sorted { order.clone() } else { (0..docs.len()).collect() };
            let input: Vec<&String> = idx.iter().map(|&i| &docs[i]).collect();
            let t = Instant::now();
            let out = match m.te.embed(&input, Some(bs)) {
                Ok(o) => o,
                Err(e) => { println!("  batch {bs}: {e}"); return; }
            };
            let rate = 128.0 / t.elapsed().as_secs_f64();
            let drift: Vec<f32> = idx.iter().zip(&out).map(|(&i, v)| cos(&single[i], v)).collect();
            let min = drift.iter().cloned().fold(1.0, f32::min);
            let mean = drift.iter().sum::<f32>() / drift.len() as f32;
            println!("  batch {bs:2} {}  {rate:6.1} docs/s   cos(batched, single) min {min:.5} mean {mean:.5}",
                if sorted { "sorted  " } else { "unsorted" });
        }
    }

    if let Some(r) = reference {
        let mut rm = load(r);
        let refv: Vec<Vec<f32>> = docs.iter().map(|d| rm.te.embed(vec![d], None).unwrap().remove(0)).collect();
        let agree: Vec<f32> = single.iter().zip(&refv).map(|(a, b)| cos(a, b)).collect();
        println!("  vs {}: cos min {:.4} mean {:.4}", r.display(),
            agree.iter().cloned().fold(1.0, f32::min), agree.iter().sum::<f32>() / agree.len() as f32);
    }
}
