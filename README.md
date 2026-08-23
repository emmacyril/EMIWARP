# EmiWarp

> A fork of Warp that runs on your own models and never talks to Warp's servers.

A fork of [warpdotdev/warp](https://github.com/warpdotdev/warp) that runs entirely
on infrastructure you control.

Upstream Warp open-sourced its client on 2026-04-28 under AGPL-3.0 (the
`warpui` / `warpui_core` crates are MIT). Its cloud agent platform, **Oz**, stayed
proprietary. EmiWarp keeps the terminal and replaces the cloud dependency with a
provider you configure.

---

## What changed, and why it's small

Warp's client reaches models two different ways:

| Path | How it works | EmiWarp |
| --- | --- | --- |
| **Cloud** | Client `POST`s to `app.warp.dev`; the agent loop runs server-side and bills the account | Removed |
| **Local harness** | Warp spawns a local agent CLI (`claude`, `codex`, `gemini`) as a child process; credentials never leave the machine | **This is what EmiWarp uses** |

Because upstream already has the local-harness path, EmiWarp does not need a new
HTTP client, a new agent loop, or a rewritten AI stack. It needs the local path
to be the *only* path, pointed wherever you want. That is why the diff against
upstream is seven one-line integrations rather than a rewrite.

There is **no account, tier, plan, or subscription concept anywhere in this
build**. Not bypassed — absent. Nothing reaches a server that would ask.

---

## Structural map

### Files EmiWarp owns (upstream has no version — never conflict)

| Path | Role |
| --- | --- |
| `crates/emiwarp/src/config.rs` | `.env.emiwarp` loader; env > file > defaults |
| `crates/emiwarp/src/provider.rs` | Provider profiles, wire schemas, harness env contract |
| `crates/emiwarp/src/identity.rs` | Local principal + local capabilities |
| `crates/emiwarp/src/egress.rs` | Vendor-host classifier (fail-closed backstop) |
| `crates/emiwarp/src/lib.rs` | Process-wide runtime + call-site shims |
| `scripts/emiwarp/overlay.py` | The seven integrations, declaratively |
| `scripts/sync_and_build.sh` | Upstream sync + release build |
| `.env.emiwarp.example` | Config template |

### Upstream files touched (one anchored line each)

| File | Injection | Effect |
| --- | --- | --- |
| `app/src/root_view.rs` | `workspace-boot` | Renders the terminal instead of the login splash |
| `app/src/ai/agent_sdk/driver.rs` | `provider-env` | Repoints the spawned agent CLI at your endpoint |
| `crates/http_client/src/lib.rs` | `egress-guard` | Null-routes any request to a Warp-operated host |
| `crates/warp_core/src/channel/mod.rs` | `brand-cli-name`, `brand-ctrl-name` | `emiwarp`, `emiwarpctrl` |
| `crates/warp_core/src/channel/state.rs` | `brand-app-id` | App identity `dev.emiwarp.EmiWarp` |
| `app/Cargo.toml` | `binary-target` | Emits a native `emiwarp` binary |

`scripts/emiwarp/overlay.py --list` prints this from the source of truth.

### Upstream behaviour EmiWarp relies on rather than patches

The OSS channel (`ChannelState::init()`) already ships with:

```rust
telemetry_config:        None,
autoupdate_config:       None,
crash_reporting_config:  None,
```

Telemetry, autoupdate, and crash reporting are therefore *already* compiled out —
no patch needed. `sync_and_build.sh` asserts all three on every sync, so an
upstream change that reintroduced any of them fails the build instead of
silently shipping.

---

## Configuration

Copy `.env.emiwarp.example` to `.env.emiwarp` (repo root) or
`~/.config/emiwarp/.env.emiwarp`.

```bash
EMIWARP_AI_PROVIDER=ollama          # ollama | openai | anthropic | gemini | openai_compatible
EMIWARP_BASE_URL=http://127.0.0.1:11434/v1
EMIWARP_API_KEY=                    # required for openai / anthropic / gemini
EMIWARP_MODEL_NAME=llama3.1:8b
```

### How provider substitution works

Every supported agent CLI reads its endpoint and credentials from environment
variables. `ProviderProfile::harness_env()` produces that overlay, and
`provider-env` applies it immediately before the spawned process's environment is
frozen — so it wins over anything upstream derived.

| Provider | CLI | Environment applied |
| --- | --- | --- |
| `anthropic` | `claude` | `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`, `ANTHROPIC_MODEL` |
| `openai` | `codex` | `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `OPENAI_MODEL` |
| `gemini` | `gemini` | `GEMINI_API_KEY`, `GOOGLE_GEMINI_BASE_URL`, `GEMINI_MODEL` |
| `ollama` | `codex` | `OPENAI_*` pointed at the Ollama daemon |
| `openai_compatible` | `codex` | `OPENAI_*` pointed at your server |

No CLI is patched. Ollama, vLLM, LM Studio, llama.cpp, LiteLLM, OpenRouter,
Together and Groq all speak OpenAI Chat Completions, so they all work through the
same path.

---

## Syncing with upstream

```bash
scripts/sync_and_build.sh              # sync, verify, build
scripts/sync_and_build.sh --dry-run    # report only
scripts/sync_and_build.sh --no-build   # sync and verify only
```

### Why there is no merge

EmiWarp's changes are **regenerated, not merged**. Each sync:

1. resets the tree to pristine `upstream/master`,
2. restores the EmiWarp-owned paths (which upstream has no version of),
3. re-runs `overlay.py` to re-apply the seven integrations,
4. asserts the invariants, runs the unit tests, then builds.

A textual merge conflict is structurally impossible, because no merge happens.
The only thing that can fail is an *anchor moving* — and when that happens the
script names the injection and the file, and stops.

### On `git merge -X ours`

The script deliberately never uses it. `-X ours` resolves conflicting hunks in
our favour, and the hunks most likely to conflict are the ones upstream just
changed — the fixes you are syncing for. It keeps the build green by quietly
discarding upstream work, including security fixes. This is the failure mode that
rots a long-lived fork, and it is invisible until it matters. EmiWarp fails loudly
instead: a five-minute re-anchor beats months of silent divergence.

### When an anchor moves

```
[FAIL] provider-env   anchor not found in app/src/ai/agent_sdk/driver.rs
       anchor: 'let resolved_env_vars = Arc::new(env_vars);'
```

Open the file, find where the construct moved, update that injection's `anchor`
in `scripts/emiwarp/overlay.py`, re-run. Each anchor is one distinctive line, so
this is normally a one-line edit.

---

## Quick start

This repository holds only EmiWarp's own sources — roughly 150 KB. It carries
none of upstream's history and none of its Git LFS assets. The sync script
clones upstream into a disposable build tree on first run.

```bash
git clone https://github.com/emmacyril/EMIWARP.git
cd EMIWARP
cp .env.emiwarp.example .env.emiwarp     # then edit it

scripts/sync_and_build.sh                # clones upstream, overlays, builds
```

First run clones warpdotdev/warp into `.emiwarp-build/` (a few minutes and a few
hundred MB). Later runs just fetch and re-apply.

The binary lands at `.emiwarp-build/target/release/emiwarp`. Point the build tree
elsewhere with `EMIWARP_WORKDIR=/path/to/tree`.

```bash
scripts/sync_and_build.sh --dry-run      # report only
scripts/sync_and_build.sh --no-build     # sync and verify only
scripts/emiwarp/overlay.py --list        # describe every injection
```

### Requirements

`git`, `python3`, and a Rust toolchain. Upstream pins **Rust 1.92.0** via
`rust-toolchain.toml`; a Homebrew rustc will not do, so install rustup. On macOS
the build also needs Xcode's Metal toolchain for shader compilation:

```bash
xcodebuild -downloadComponent MetalToolchain
```

`./script/bootstrap` (run automatically) installs the remaining native deps.

### Two layouts

The script auto-detects which it is in:

| Layout | When | Build tree |
| --- | --- | --- |
| **overlay** (this repo) | no `app/Cargo.toml` present | `.emiwarp-build/` |
| **in-tree** | repo *is* a warp checkout | the repo itself |

---

## Licensing

Derived from warpdotdev/warp: **AGPL-3.0-only**, except `warpui` / `warpui_core`
(MIT). Practically:

- Building and running EmiWarp locally carries no publication obligation.
- **Distributing an EmiWarp binary obliges you to publish the complete
  corresponding source under AGPL-3.0**, including these modifications.
- **Running it as a network service triggers the same obligation** (AGPL §13),
  which is the clause that separates AGPL from GPL.
- "Warp" and its logos are trademarks and are *not* licensed by the AGPL. The
  rebrand is what keeps this fork clear of them — don't reintroduce upstream
  branding into a build you distribute.
