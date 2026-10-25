# Research: secret detection for `write` batches

Ticket: #20. Context: SPEC §5 check 2 ("a pattern- and entropy-based detector (API keys, tokens, private keys, credentials in URLs). A match rejects the whole batch with an explanation."), ADR-0003 (Rust, single dependency-free binary), ADR-0004 (the brain is a git repo pushed to a remote, so a leaked secret persists in history).

Researched 2026-10-25. Every version, count and licence below was read from the primary source on that date. The empirical results come from running each tool against the fixtures described in [Empirical test](#empirical-test).

## Recommendation

**Build the detector in-process with the `regex` crate. Take the provider-specific rules from Betterleaks' MIT-licensed rule file (`config/betterleaks.toml`), pinned and vendored at build time. Write our own generic "keyword + random value" rule and credential-in-URL rule, each with a brain-specific allowlist for values the brain legitimately holds: git SHAs, `sha256:` Anchor hashes, ULIDs and UUIDs.**

- Do not depend on a scanner crate. The only mature embeddable one, `kingfisher-scanner`, brings in a native Vectorscan (Hyperscan) build. That cuts against ADR-0003, and its quality advantage is mostly something we can reproduce (see below).
- Do not use TruffleHog's detectors (AGPL-3.0) or secrets-patterns-db (CC-BY-SA-4.0).
- Leave schema-validated structured fields out of the secret scan: `id`, `supersedes`, `anchors[].commit`, `anchors[].hash`, timestamps. The schema check already constrains their shape. Scan free text: `title`, `summary`, `tags`, body, and Entity fields written through `update_entity`.

## Options compared

| Option | Licence | Form | Status (2026-10-25) | Verdict |
|---|---|---|---|---|
| **Betterleaks rules** (`betterleaks/betterleaks`, `config/betterleaks.toml`) | MIT, "Copyright (c) 2026 Zachary Rice" | Generated TOML: 466 rules (391 high, 72 medium, 3 low confidence). Each rule has `regex`, `keywords` and `confidence`. Filters and validators are Expr-language programs | Latest release v1.9.0 (2026-09-29). Config last changed 2026-10-13 (`c613797`) | **Use as the rule source.** Take regex and keywords from high/medium rules. Ignore the `filter`/`validate` Expr programs |
| **Gitleaks rules** (`gitleaks/gitleaks`, `config/gitleaks.toml`) | MIT, "Copyright (c) 2019 Zachary Rice" | Generated TOML: 222 rules, purely declarative (`regex`, `entropy`, `keywords`, `allowlists`, `stopwords`) | Latest release v8.30.1 (2026-03-21). Config last changed 2025-11-20 (`09242ce`) | Fallback source. Easier to port, but the rule set has stopped growing since its author moved to Betterleaks |
| **`kingfisher-scanner`** crate (MongoDB) | Apache-2.0 (scanner). `kingfisher-rules` is "Apache-2.0 AND MIT" | Embeddable Rust lib: `Scanner::scan_bytes(&[u8])`, 488 built-in rules converted from Betterleaks and Google Veles | v2.0.0 on crates.io (2026-10-20). Repo release v2.14.0. Edition 2024, `rust-version = "1.99"` | Best out-of-box precision in our test. But it needs **Vectorscan**, a native dependency whose build "may download a platform archive", and that brings a large dependency surface. Keep as plan B |
| **`ripsecrets`** (sirwart) | MIT | CLI crate. The public fn `find_secrets(paths, …, BufferWriter)` works on file paths, not strings. About 30 patterns plus a `p_random` randomness test | v0.1.11, crates.io updated 2025-05-27 | Not usable as a library. Its `p_random` idea (MIT) is worth porting if Shannon entropy proves too weak |
| **TruffleHog** | **AGPL-3.0** | Go detectors in code | Active | **Excluded.** Copying its patterns into an MIT binary would impose AGPL terms |
| **secrets-patterns-db** (mazen160) | **CC-BY-SA-4.0** | YAML regex list | Last push 2025-08-06 | **Excluded.** ShareAlike would attach to the derived rule set |
| **detect-secrets** (Yelp) | Apache-2.0 | Python plugins | Last push 2026-04-02 | Wrong language. Nothing to gain over Betterleaks |
| Other crates.io hits (`secrets_scanner`, `keyhog-core`, `secretscan`, `rustleaks-core`…) | mostly MIT | Small, young, low-download projects | — | Not worth a dependency for a security-critical check |

Nosey Parker (`praetorian-inc/noseyparker`, Apache-2.0) is **archived**. Kingfisher's NOTICE says Kingfisher derives from it but no longer takes its detector catalogue from it.

### Licence compatibility (project presumed MIT; the repo has no LICENSE file yet)

- **MIT rules (Betterleaks, Gitleaks):** compatible. We must keep the copyright and permission notice. Ship it in a `THIRD_PARTY_NOTICES` file, and include it in the binary's `--licenses` output if we have one.
- **Apache-2.0 (Kingfisher, detect-secrets):** can be combined into an MIT-licensed project. We would have to carry their NOTICE files and licence text.
- **AGPL-3.0 (TruffleHog) and CC-BY-SA-4.0 (secrets-patterns-db):** not compatible with distributing the brain as a plain MIT binary.

## Does the Go rule syntax port to Rust's `regex`?

Yes, with small mechanical fixes. Go's RE2 and Rust's `regex` both exclude lookaround and backreferences. A grep of both rule files found no lookaround (`(?=`, `(?!`, `(?<=`) and no backreferences. I compiled every `regex`, `path` and `regexes` entry with `regex` 1.13.1 inside a `rust:1-slim` container:

| File | Patterns | Compile as-is | Failures |
|---|---|---|---|
| `betterleaks.toml` @ `c613797` | 470 | 468 | `pypi-upload-token` and `vault-batch-token` exceed the default 10 MiB compiled-size limit. Both compile with `RegexBuilder::size_limit(64 MiB)` |
| `gitleaks.toml` @ `09242ce` | 275 | 270 | The same 2, plus `generic-api-key` (size limit). 4 more patterns rely on Go treating an unescaped `{` as a literal (`^\${(?:…)}$`, `\$?{{[^}]+}}`, `import[ \t]+{…}`), and Rust rejects them. Escaping the `{` fixes them |

Neither file uses Go-only syntax beyond that. Named groups `(?P<x>…)`, scoped flags `(?-i:…)`, `\x60` and `\z` are all accepted by `regex`.

## False-positive risk on what the brain legitimately holds

### Empirical test

There were two fixtures. `benign.md` is a realistic Recollection: front-matter with a ULID `id`/`supersedes`, an Anchor with a 40-hex commit and a `sha256:` hash, a body with short and full SHAs, an example `.env` block with placeholder credentials, a ULID and a SHA under names containing `token`/`key`/`secret`, a UUID, and AWS's documented `AKIAIOSFODNN7EXAMPLE` key. `secret.md` holds realistic but fake secrets: a GitHub PAT, an AWS key pair, a Postgres URL with a real-looking password, a Stripe live key, an OpenSSH private key and a Slack webhook.

| Tool (default config) | True positives (of 7) | False positives in `benign.md` |
|---|---|---|
| Gitleaks v8.30.1 | 6. It missed the password in the Postgres URL | **5**, all from `generic-api-key`: `cache_key = "<sha1>"`, `auth_token_id = "<ULID>"`, `session_token = "<ULID>"`, `deploy_key_commit = "<sha>"`, `secret_sha: <sha>` |
| Betterleaks v1.9.0 | 6 (the AWS secret is only a dependent capture) | **7**: the same 5, plus `generic-credential-uri` on the placeholder URLs `user:password@localhost` and `postgres:postgres@localhost` |
| Kingfisher v2.14.0, `--confidence medium` (default) | 5 (missed the URL password; the AWS secret is a dependent capture) | **0** |
| Kingfisher v2.14.0, `--confidence low` | 6 | 2 (the placeholder URLs) |
| ripsecrets v0.1.11 | 7 | **5** (the same hex/ULID values under keyword names) |

None of the tools flagged the front-matter `commit:`/`hash:` lines, bare SHAs in prose, the UUID, `${GITHUB_TOKEN}`, `your-api-key-here`, `changeme` or AWS's `…EXAMPLE` keys. The provider rules carry stopword and placeholder allowlists for those.

### Why entropy alone cannot separate them

Shannon entropy (bits per character), as Gitleaks' `entropy` field defines it ("the minimum shannon entropy a regex group must have"):

| Value | Length | Entropy |
|---|---|---|
| git SHA-1 (`3f2a9c1e…a39`) | 40 | 3.94 |
| SHA-1 (`da39a3ee…709`) | 40 | 3.74 |
| sha256 Anchor hash | 64 | 3.79 |
| ULID | 26 | 4.47 |
| UUID | 36 | 3.39 |
| GitHub PAT | 40 | 5.12 |
| AWS secret key | 40 | 5.17 |
| URL password `Xk9mQ2vP7rL4tB8w` | 16 | 4.00 |
| `your-api-key-here` | 17 | 3.29 |

Hex tops out at log2(16) = 4.0 bits, so it clears the 3.5 threshold that the generic rule uses in both Gitleaks and Betterleaks. ULIDs clear it easily. The brain's own identifiers look exactly like secrets to an entropy test. The fix is to **recognise their shape**, not to tune the threshold.

The risk is higher for this brain than for a typical repo. Recollections are prose *about* code, and "key", "token", "secret", "auth" and "credential" are ordinary words in them (idempotency keys, cache keys, token refresh, auth flows). The generic rule's keyword prefilter will fire often.

### Mitigations to build in

1. **Leave schema-validated fields out of the secret scan.** The schema check already constrains ULIDs, commits and `sha256:` hashes in front-matter.
2. **Brain-shape allowlist for generic rules only.** Skip a generic-rule candidate whose captured value is entirely one of:
   - hex of length 7–40 or 64 (git object IDs, SHA-1/SHA-256)
   - `sha256:<64 hex>`
   - a ULID (`^[0-7][0-9A-HJKMNP-TV-Z]{25}$`)
   - a UUID
   Provider-specific rules (prefix-anchored, such as `ghp_` or `AKIA`) are *not* allowlisted.
   - Residual risk: a raw hex secret under a `secret =` name will slip through. Some legacy APIs use bare 32/40-hex keys. We accept that risk, because provider rules still catch the prefixed formats that dominate today.
3. **Placeholder allowlist for credentials in URLs.** Skip the password when it is one of `password`, `pass`, `secret`, `changeme`, `example`, `postgres`, `root`, `admin`, the username repeated, or a `${VAR}`/`<VAR>`/`{{ var }}` template. Also skip it when the host is `localhost`, `127.0.0.1` or `*.example.*`. Port the global placeholder regexes and stopwords from `gitleaks.toml`.
4. **Use only high- and medium-confidence provider rules** from Betterleaks, as Kingfisher's default does. That default produced zero false positives in the test.
5. **Make rejections actionable.** Name the rule ID, the operation index, the field and line, and a redacted preview such as `ghp_R7…E9u`, so the Agent can rephrase: abbreviate the SHA, use a placeholder. Do not echo the full value.
6. **Optional upgrade:** if the generic rule stays noisy, add Betterleaks' "token efficiency" (BPE rarity) filter. Its README says it uses "BPE tokenization to measure how 'rare' or non-human a string is". The alternative is to port ripsecrets' MIT `p_random` (distinct characters, character classes, bigrams). Either is a post-v1 refinement.

## Implementation sketch

- `build.rs` or a checked-in generated `rules.rs`: parse a pinned `betterleaks.toml` and keep the `id`, `regex`, `keywords` and `confidence` of rules at high/medium confidence. Record the upstream commit SHA in the file header and the MIT notice in `THIRD_PARTY_NOTICES`.
- At runtime: lowercase the text and run a keyword prefilter (`aho-corasick`, already a dependency of `regex`). Run only the rules whose keywords appear. A batch is a few KB, so per-rule `Regex::find_iter` is fast enough, and no `RegexSet` or Vectorscan is needed. Compile the rules lazily (`std::sync::LazyLock`) so MCP startup does not pay for them until the first `write`.
- Add our own rules: a generic keyword-plus-value rule (entropy ≥ 3.5 plus the brain-shape allowlist), credential-in-URL (with the placeholder allowlist), and PEM/OpenSSH/PGP private-key blocks.
- Fixtures: run `benign.md` and `secret.md` (above) as regression tests, plus the `examples`/`negative` cases from the upstream rule files where present.

## Sources

- SPEC §5, ADR-0003 and ADR-0004 in this repo.
- Gitleaks: https://github.com/gitleaks/gitleaks. LICENSE (MIT, 2019 Zachary Rice). `config/gitleaks.toml` @ `09242ce9c8a60d9b051fc2d166f9e849b88c7ac0`. README config reference (entropy is "minimum shannon entropy"; keywords are a "pre-regex check"; stopwords "target the extracted secret"). Release v8.30.1.
- Betterleaks: https://github.com/betterleaks/betterleaks. LICENSE (MIT, 2026 Zachary Rice). `config/betterleaks.toml` @ `c613797a684337a014168063577bb6fa6e3b6296`. README (BPE filtering, Expr filters and validation). Release v1.9.0.
- Kingfisher: https://github.com/mongodb/kingfisher. `Cargo.toml` (workspace, `kingfisher-vectorscan`, `rust-version = "1.99"`). `NOTICE` (Nosey Parker Apache-2.0, Betterleaks MIT, Veles Apache-2.0). `crates/kingfisher-scanner` README (the `scan_bytes` API and the Vectorscan build notes). crates.io: `kingfisher-scanner` 2.0.0 (Apache-2.0), `kingfisher-rules` 2.0.0 ("Apache-2.0 AND MIT").
- ripsecrets: https://github.com/sirwart/ripsecrets. `Cargo.toml` (MIT, 0.1.11). `src/lib.rs` (pattern list, path-based `find_secrets`). `src/matcher/p_random.rs`.
- TruffleHog: https://github.com/trufflesecurity/trufflehog (licence AGPL-3.0, from the GitHub API).
- secrets-patterns-db: https://github.com/mazen160/secrets-patterns-db (licence CC-BY-SA-4.0, from the GitHub API).
- detect-secrets: https://github.com/Yelp/detect-secrets (Apache-2.0). Nosey Parker: https://github.com/praetorian-inc/noseyparker (Apache-2.0, archived).
- Background on Betterleaks as Gitleaks' successor (secondary, used only for context): BleepingComputer, "Betterleaks, a new open-source secrets scanner to replace Gitleaks" (March 2026).
- Empirical runs: container images `ghcr.io/gitleaks/gitleaks:v8.30.1`, `ghcr.io/betterleaks/betterleaks:v1.9.0` and `ghcr.io/mongodb/kingfisher:latest` (v2.14.0), and the ripsecrets v0.1.11 release binary. Regex compilation used `regex` 1.13.1 and `toml` 0.8 in `rust:1-slim`.
