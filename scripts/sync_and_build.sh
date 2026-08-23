#!/usr/bin/env bash
#
# EmiWarp — upstream sync and release build.
#
# Rebuilds EmiWarp on top of the latest warpdotdev/warp without merging.
#
# Two layouts, auto-detected
# --------------------------
#   overlay repo (default)  This repository holds only EmiWarp's own sources.
#                           Upstream is cloned into a build directory
#                           ($EMIWARP_WORKDIR, default .emiwarp-build/) and the
#                           overlay is applied there. Nothing of upstream's is
#                           vendored, so the repo stays small and carries none
#                           of upstream's Git LFS assets.
#
#   in-tree                 This repository *is* a warp checkout with EmiWarp
#                           files overlaid. Used when app/Cargo.toml is present.
#
# Why there is no merge
# ---------------------
# EmiWarp's changes come in two forms, and neither is merged:
#
#   1. Files upstream does not have — crates/emiwarp/, scripts/emiwarp/, this
#      script, the config template. Upstream never touches them, so they are
#      simply carried across.
#
#   2. Seven one-line integrations into upstream files. These are not stored as
#      diffs; they are *regenerated* on each sync by scripts/emiwarp/overlay.py,
#      which anchors on a single distinctive line per site.
#
# So the tree is upstream's tree, plus our files, plus a re-applied overlay. A
# textual merge conflict is structurally impossible. The one thing that can fail
# is an anchor moving, which the overlay reports by name and line — and when that
# happens this script stops rather than guessing.
#
# What this deliberately does NOT do
# ----------------------------------
# It never runs `git merge -X ours`. That strategy resolves conflicting hunks in
# our favour, and the hunks most likely to conflict are the ones upstream just
# changed — the fixes we are syncing for. It would keep the build green while
# silently discarding upstream work, including security fixes. A loud failure is
# worth more than a quiet divergence.
#
# Usage:  scripts/sync_and_build.sh [options]
#   --no-build          sync and verify only
#   --no-bootstrap      skip ./script/bootstrap
#   --dry-run           report what would change; touch nothing
#   --ref <ref>         upstream ref to sync to (default: remote's default branch)
#   --jobs <n>          cargo -j
#   -y, --yes           non-interactive
#   -h, --help
#
set -Eeuo pipefail

# Note: macOS ships bash 3.2, where expanding an empty array as "${a[@]}" trips
# `set -u`. Empty-able arrays below use the ${a[@]+"${a[@]}"} guard.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

UPSTREAM_URL="https://github.com/warpdotdev/warp.git"
UPSTREAM_REMOTE="upstream"
UPSTREAM_REF=""          # empty => auto-detect the remote default branch
WORK_BRANCH="emiwarp/main"
BIN_NAME="emiwarp"

DO_BUILD=1
DO_BOOTSTRAP=1
DRY_RUN=0
ASSUME_YES=0
JOBS=""

# Paths EmiWarp owns. Preserved verbatim across a sync; upstream has no version
# of these, so they can never conflict.
EMIWARP_OWNED=(
  "crates/emiwarp"
  "scripts/emiwarp"
  "scripts/sync_and_build.sh"
  ".env.emiwarp.example"
  "EMIWARP.md"
  # Upstream ships its own README.md; ours must win on every sync.
  "README.md"
)

# ---------------------------------------------------------------------------
# output
# ---------------------------------------------------------------------------
if [[ -t 1 ]]; then
  C_RESET=$'\033[0m'; C_BOLD=$'\033[1m'; C_DIM=$'\033[2m'
  C_RED=$'\033[31m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'; C_BLUE=$'\033[34m'
else
  C_RESET=""; C_BOLD=""; C_DIM=""; C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""
fi

step()  { printf '%s==>%s %s%s%s\n' "$C_BLUE" "$C_RESET" "$C_BOLD" "$*" "$C_RESET"; }
info()  { printf '    %s\n' "$*"; }
dim()   { printf '    %s%s%s\n' "$C_DIM" "$*" "$C_RESET"; }
warn()  { printf '%s[warn]%s %s\n' "$C_YELLOW" "$C_RESET" "$*" >&2; }
ok()    { printf '%s  ok  %s %s\n' "$C_GREEN" "$C_RESET" "$*"; }
die()   { printf '%s[fail]%s %s\n' "$C_RED" "$C_RESET" "$*" >&2; exit 1; }

STAGE="startup"
on_err() {
  local code=$?
  printf '\n%s[fail]%s aborted during: %s (exit %d)\n' "$C_RED" "$C_RESET" "$STAGE" "$code" >&2
  if [[ -n "${STASH_REF:-}" ]]; then
    warn "local changes are preserved in stash: $STASH_REF"
    warn "restore with: git stash pop '$STASH_REF'"
  fi
  if [[ -n "${QUARANTINE_BRANCH:-}" ]] && git rev-parse --verify -q "$QUARANTINE_BRANCH" >/dev/null; then
    warn "partial sync preserved on branch: $QUARANTINE_BRANCH"
  fi
  exit "$code"
}
trap on_err ERR

confirm() {
  (( ASSUME_YES )) && return 0
  [[ -t 0 ]] || return 0
  local reply
  read -r -p "    $1 [y/N] " reply
  [[ "$reply" =~ ^[Yy]$ ]]
}

# ---------------------------------------------------------------------------
# args
# ---------------------------------------------------------------------------
while (( $# )); do
  case "$1" in
    --no-build)     DO_BUILD=0; shift ;;
    --no-bootstrap) DO_BOOTSTRAP=0; shift ;;
    --dry-run)      DRY_RUN=1; shift ;;
    --ref)          UPSTREAM_REF="${2:?--ref needs a value}"; shift 2 ;;
    --jobs)         JOBS="${2:?--jobs needs a value}"; shift 2 ;;
    -y|--yes)       ASSUME_YES=1; shift ;;
    -h|--help)      sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)              die "unknown option: $1 (try --help)" ;;
  esac
done

# ---------------------------------------------------------------------------
STAGE="preflight"
step "Preflight"

for tool in git python3; do
  command -v "$tool" >/dev/null 2>&1 || die "required tool not found: $tool"
done
if (( DO_BUILD )); then
  command -v cargo >/dev/null 2>&1 || die "cargo not found; install Rust or pass --no-build"
fi
git rev-parse --git-dir >/dev/null 2>&1 || die "not a git repository: $REPO_ROOT"

[[ -f scripts/emiwarp/overlay.py ]] || die "scripts/emiwarp/overlay.py is missing"
[[ -d crates/emiwarp ]]             || die "crates/emiwarp is missing"

# An in-tree checkout has upstream's app crate; an overlay repo does not.
if [[ -f app/Cargo.toml ]]; then
  LAYOUT="in-tree"
  WORKDIR="$REPO_ROOT"
else
  LAYOUT="overlay"
  WORKDIR="${EMIWARP_WORKDIR:-$REPO_ROOT/.emiwarp-build}"
fi
ok "layout: $LAYOUT"
[[ "$LAYOUT" == "overlay" ]] && info "build tree: $WORKDIR"

ok "git $(git --version | awk '{print $3}'), python3 $(python3 -c 'import sys;print(".".join(map(str,sys.version_info[:3])))')"
(( DO_BUILD )) && ok "cargo $(cargo --version | awk '{print $2}')"

# ---------------------------------------------------------------------------
STAGE="resolving upstream"

if [[ "$LAYOUT" == "overlay" ]]; then
  # No upstream remote is added to this repository: the disposable build tree
  # owns that relationship. Resolve refs straight off the wire so this works
  # before the build tree exists, and under --dry-run.
  step "Resolving upstream"

  if [[ -z "$UPSTREAM_REF" ]]; then
    UPSTREAM_REF="$(git ls-remote --symref "$UPSTREAM_URL" HEAD 2>/dev/null \
      | awk '/^ref:/ {sub("refs/heads/","",$2); print $2; exit}')"
    [[ -z "$UPSTREAM_REF" ]] && UPSTREAM_REF="master"
    dim "default branch resolved to '$UPSTREAM_REF'"
  fi

  UPSTREAM_SHA="$(git ls-remote "$UPSTREAM_URL" "refs/heads/$UPSTREAM_REF" 2>/dev/null \
    | awk '{print $1; exit}')"
  [[ -n "$UPSTREAM_SHA" ]] || die "could not resolve $UPSTREAM_URL ref '$UPSTREAM_REF' (offline?)"
  UPSTREAM_SHORT="${UPSTREAM_SHA:0:8}"
  ok "warpdotdev/warp $UPSTREAM_REF at $UPSTREAM_SHORT"
else
  step "Upstream remote"

  if git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1; then
    current="$(git remote get-url "$UPSTREAM_REMOTE")"
    if [[ "$current" != "$UPSTREAM_URL" ]]; then
      warn "remote '$UPSTREAM_REMOTE' points at $current"
      if confirm "repoint it to $UPSTREAM_URL?"; then
        (( DRY_RUN )) || git remote set-url "$UPSTREAM_REMOTE" "$UPSTREAM_URL"
      fi
    fi
    ok "$UPSTREAM_REMOTE -> $current"
  else
    info "adding remote '$UPSTREAM_REMOTE' -> $UPSTREAM_URL"
    (( DRY_RUN )) || git remote add "$UPSTREAM_REMOTE" "$UPSTREAM_URL"
    ok "remote added"
  fi

  if [[ -z "$UPSTREAM_REF" ]]; then
    UPSTREAM_REF="$(git symbolic-ref -q --short "refs/remotes/$UPSTREAM_REMOTE/HEAD" 2>/dev/null \
      | sed "s#^$UPSTREAM_REMOTE/##")"
    if [[ -z "$UPSTREAM_REF" ]]; then
      UPSTREAM_REF="$(git remote show "$UPSTREAM_REMOTE" 2>/dev/null \
        | awk '/HEAD branch:/ {print $NF}')"
    fi
    [[ -z "$UPSTREAM_REF" ]] && UPSTREAM_REF="master"
    dim "default branch resolved to '$UPSTREAM_REF'"
  fi

  step "Fetching $UPSTREAM_REMOTE/$UPSTREAM_REF"
  if (( DRY_RUN )); then
    dim "(dry run) would fetch"
  else
    git fetch --tags --prune "$UPSTREAM_REMOTE" "$UPSTREAM_REF"
  fi

  git rev-parse -q --verify "$UPSTREAM_REMOTE/$UPSTREAM_REF" >/dev/null \
    || die "no such upstream ref: $UPSTREAM_REMOTE/$UPSTREAM_REF"

  UPSTREAM_SHA="$(git rev-parse "$UPSTREAM_REMOTE/$UPSTREAM_REF")"
  UPSTREAM_SHORT="$(git rev-parse --short "$UPSTREAM_REMOTE/$UPSTREAM_REF")"
  ok "$UPSTREAM_REMOTE/$UPSTREAM_REF at $UPSTREAM_SHORT"
  dim "$(git log -1 --format='%s' "$UPSTREAM_SHA")"
fi

BASE_SHA="$(git rev-parse -q --verify "refs/emiwarp/last-sync" 2>/dev/null || true)"
if [[ -n "$BASE_SHA" ]]; then
  if [[ "$BASE_SHA" == "$UPSTREAM_SHA" ]]; then
    ok "already at the newest upstream commit"
  else
    info "upstream has moved since the last sync"
  fi
fi

# ---------------------------------------------------------------------------
STAGE="staging emiwarp-owned files"
step "Preserving EmiWarp sources"

STAGING="$(mktemp -d "${TMPDIR:-/tmp}/emiwarp-sync.XXXXXX")"
cleanup_staging() { [[ -n "${STAGING:-}" && -d "$STAGING" ]] && rm -rf "$STAGING"; }
trap 'cleanup_staging' EXIT

preserved=0
for path in "${EMIWARP_OWNED[@]}"; do
  if [[ -e "$path" ]]; then
    mkdir -p "$STAGING/$(dirname "$path")"
    cp -R "$path" "$STAGING/$path"
    preserved=$(( preserved + 1 ))
  fi
done
ok "$preserved EmiWarp path(s) staged"

# ---------------------------------------------------------------------------
STAGE="protecting local work"
step "Local working tree"

# Runs *after* EmiWarp sources are staged: they are often untracked, and
# `--include-untracked` would otherwise sweep them into the stash and leave the
# overlay with nothing to apply.
STASH_REF=""
if [[ -n "$(git status --porcelain)" ]]; then
  if (( DRY_RUN )); then
    dim "(dry run) would stash local changes"
  else
    info "stashing uncommitted changes"
    git stash push --include-untracked \
      --message "emiwarp-sync $(date -u +%Y-%m-%dT%H:%M:%SZ)" >/dev/null
    STASH_REF="$(git rev-parse stash@{0})"
    ok "stashed at ${STASH_REF:0:12} (restore: git stash pop)"
  fi
else
  ok "working tree clean"
fi

# ---------------------------------------------------------------------------
STAGE="preparing build tree"
step "Rebuilding on $UPSTREAM_SHORT"

QUARANTINE_BRANCH=""

if [[ "$LAYOUT" == "overlay" ]]; then
  # The build tree is a plain upstream checkout that this repo never tracks.
  # It is disposable: everything EmiWarp contributes is copied in below.
  if (( DRY_RUN )); then
    dim "(dry run) would sync upstream into $WORKDIR and apply the overlay"
  else
    if [[ ! -d "$WORKDIR/.git" ]]; then
      info "cloning upstream into $WORKDIR (first run; this takes a while)"
      git clone --filter=blob:none "$UPSTREAM_URL" "$WORKDIR"
    fi
    git -C "$WORKDIR" remote set-url origin "$UPSTREAM_URL" 2>/dev/null || true
    git -C "$WORKDIR" fetch --prune origin "$UPSTREAM_REF"
    git -C "$WORKDIR" checkout -q -B "$WORK_BRANCH" \
      "$(git -C "$WORKDIR" rev-parse "origin/$UPSTREAM_REF")"
    git -C "$WORKDIR" reset -q --hard "origin/$UPSTREAM_REF"
    git -C "$WORKDIR" clean -qfd -e .emiwarp-build

    for path in "${EMIWARP_OWNED[@]}"; do
      if [[ -e "$STAGING/$path" ]]; then
        rm -rf "${WORKDIR:?}/$path"
        mkdir -p "$WORKDIR/$(dirname "$path")"
        cp -R "$STAGING/$path" "$WORKDIR/$path"
      fi
    done
    ok "build tree = upstream $UPSTREAM_SHORT + EmiWarp sources"
  fi
else
  if (( DRY_RUN )); then
    dim "(dry run) would reset $WORK_BRANCH to $UPSTREAM_SHORT and restore EmiWarp sources"
  else
    PREV_SHA="$(git rev-parse -q --verify HEAD 2>/dev/null || true)"
    if [[ -n "$PREV_SHA" ]]; then
      QUARANTINE_BRANCH="emiwarp/pre-sync-$(date -u +%Y%m%d%H%M%S)"
      git branch "$QUARANTINE_BRANCH" "$PREV_SHA" >/dev/null 2>&1 || true
      dim "previous state kept on $QUARANTINE_BRANCH"
    fi

    git checkout -q -B "$WORK_BRANCH" "$UPSTREAM_SHA"

    for path in "${EMIWARP_OWNED[@]}"; do
      if [[ -e "$STAGING/$path" ]]; then
        rm -rf "$path"
        mkdir -p "$(dirname "$path")"
        cp -R "$STAGING/$path" "$path"
      fi
    done
    ok "tree = upstream $UPSTREAM_SHORT + EmiWarp sources"
  fi
fi

# ---------------------------------------------------------------------------
STAGE="applying overlay"
step "Applying EmiWarp overlay"

overlay_args=()
(( DRY_RUN )) && overlay_args+=(--dry-run)

# Under --dry-run the overlay build tree may not exist yet; there is nothing to
# apply to and nothing to report beyond that.
if (( DRY_RUN )) && [[ ! -f "$WORKDIR/Cargo.toml" ]]; then
  dim "(dry run) build tree not created yet — overlay would be applied after clone"
  python3 scripts/emiwarp/overlay.py --list | sed 's/^/    /'
  overlay_args+=(--skip)
fi

if [[ " ${overlay_args[*]-} " == *" --skip "* ]]; then
  : # build tree absent under dry-run; nothing to apply
elif ! python3 scripts/emiwarp/overlay.py --root "$WORKDIR" ${overlay_args[@]+"${overlay_args[@]}"}; then
  cat >&2 <<MSG

${C_RED}Overlay could not be fully applied.${C_RESET}

Upstream moved or reworded a line the overlay anchors on. Nothing was
force-merged and no upstream change was discarded — the sync stopped instead.

To fix:
  1. Read the FAIL line above; it names the injection and the file.
  2. Open that file and find where the anchored construct moved to.
  3. Update that injection's \`anchor\` in scripts/emiwarp/overlay.py.
  4. Re-run this script.

MSG
  [[ -n "$QUARANTINE_BRANCH" ]] && warn "pre-sync state: $QUARANTINE_BRANCH"
  exit 1
fi

if ! (( DRY_RUN )); then
  python3 scripts/emiwarp/overlay.py --root "$WORKDIR" --verify || die "overlay verification failed after apply"
fi

# ---------------------------------------------------------------------------
STAGE="invariant checks"
step "Checking EmiWarp invariants"

invariant_failed=0
if [[ ! -f "$WORKDIR/Cargo.toml" ]]; then
  dim "(dry run) build tree absent — invariants would be checked after clone"
  SKIP_INVARIANTS=1
fi
check() {
  (( ${SKIP_INVARIANTS:-0} )) && return 0
  local label="$1" cmd="$2"
  if eval "$cmd" >/dev/null 2>&1; then
    ok "$label"
  else
    printf '%s  FAIL%s %s\n' "$C_RED" "$C_RESET" "$label" >&2
    invariant_failed=1
  fi
}

# The build must not carry a telemetry sink, an autoupdater, or crash reporting.
# Upstream already sets these to None on the OSS channel; this asserts it stayed
# that way rather than trusting it.
check "telemetry sink absent on this channel" \
  "grep -q 'telemetry_config: None' $WORKDIR/crates/warp_core/src/channel/state.rs"
check "crash reporting absent on this channel" \
  "grep -q 'crash_reporting_config: None' $WORKDIR/crates/warp_core/src/channel/state.rs"
check "autoupdate absent on this channel" \
  "grep -q 'autoupdate_config: None' $WORKDIR/crates/warp_core/src/channel/state.rs"
check "emiwarp binary target present" \
  "grep -q 'name = \"emiwarp\"' app/Cargo.toml"
check "egress guard wired into http_client" \
  "grep -q 'emiwarp_block_vendor_egress' $WORKDIR/crates/http_client/src/lib.rs"
check "provider env overlay wired into agent driver" \
  "grep -q 'emiwarp::harness_env_overlay' $WORKDIR/app/src/ai/agent_sdk/driver.rs"

if (( invariant_failed )); then
  die "invariant check failed — refusing to build"
fi

# ---------------------------------------------------------------------------
STAGE="unit tests"
if (( DO_BUILD )); then
  step "Testing emiwarp crate"
  if (( DRY_RUN )); then
    dim "(dry run) would run cargo test -p emiwarp"
  else
    (cd "$WORKDIR" && cargo test -p emiwarp --quiet) || die "emiwarp unit tests failed"
    ok "emiwarp tests passed"
  fi
fi

# ---------------------------------------------------------------------------
STAGE="commit"
step "Recording sync"

if [[ "$LAYOUT" == "overlay" ]]; then
  # Nothing to commit: the build tree is disposable and this repo tracks only
  # EmiWarp's own sources, which the sync never modifies.
  (( DRY_RUN )) || git update-ref refs/emiwarp/last-sync "$UPSTREAM_SHA"
  ok "overlay layout — build tree is disposable, nothing to commit"
elif (( DRY_RUN )); then
  dim "(dry run) would commit the synced tree"
else
  git add -A
  if git diff --cached --quiet; then
    ok "nothing changed since last sync"
  else
    git commit -q -m "sync: upstream ${UPSTREAM_REF}@${UPSTREAM_SHORT}

Rebuilt on warpdotdev/warp ${UPSTREAM_SHORT} and re-applied the EmiWarp
overlay. Regenerated, not merged — see scripts/emiwarp/overlay.py.

Upstream: ${UPSTREAM_SHA}"
    ok "committed $(git rev-parse --short HEAD)"
  fi
  git update-ref refs/emiwarp/last-sync "$UPSTREAM_SHA"
fi

# ---------------------------------------------------------------------------
STAGE="bootstrap"
if (( DO_BUILD )) && (( DO_BOOTSTRAP )); then
  step "Bootstrapping toolchain"
  if (( DRY_RUN )); then
    dim "(dry run) would run ./script/bootstrap"
  elif [[ -x "$WORKDIR/script/bootstrap" ]]; then
    # Upstream's bootstrap checks `gcloud auth print-identity-token` and blocks
    # on an interactive login if it fails. That check only passes inside Warp's
    # own GCP project, so an EmiWarp build must always skip it.
    boot_args=(--skip-gcloud-auth)
    (( ASSUME_YES )) && boot_args+=(--yes)
    export WARP_SKIP_GCLOUD_AUTH=1
    export WARP_SKIP_SUDO_PROMPT="${WARP_SKIP_SUDO_PROMPT:-1}"
    (cd "$WORKDIR" && ./script/bootstrap ${boot_args[@]+"${boot_args[@]}"}) || die "./script/bootstrap failed"
    ok "bootstrap complete"
  else
    warn "script/bootstrap not found or not executable — skipping"
  fi
fi

# ---------------------------------------------------------------------------
STAGE="release build"
if (( DO_BUILD )); then
  step "Building $BIN_NAME (release)"
  if (( DRY_RUN )); then
    dim "(dry run) would run cargo build --release --bin $BIN_NAME"
  else
    build_args=(build --release --bin "$BIN_NAME")
    [[ -n "$JOBS" ]] && build_args+=(-j "$JOBS")
    (cd "$WORKDIR" && cargo "${build_args[@]}") || die "release build failed"

    ARTIFACT="$WORKDIR/target/release/$BIN_NAME"
    [[ -f "$ARTIFACT" ]] || die "build reported success but $ARTIFACT is missing"
    ok "$ARTIFACT ($(du -h "$ARTIFACT" | cut -f1))"
  fi
fi

# ---------------------------------------------------------------------------
STAGE="done"
printf '\n%s%sEmiWarp sync complete%s\n' "$C_GREEN" "$C_BOLD" "$C_RESET"
info "upstream   warpdotdev/warp $UPSTREAM_REF @ $UPSTREAM_SHORT"
[[ "$LAYOUT" == "in-tree" ]] && info "branch     $WORK_BRANCH"
(( DO_BUILD )) && ! (( DRY_RUN )) && info "binary     $WORKDIR/target/release/$BIN_NAME"
if [[ -n "$STASH_REF" ]]; then
  printf '\n'
  warn "your pre-sync local changes are still stashed"
  warn "review with: git stash show -p stash@{0}"
fi
