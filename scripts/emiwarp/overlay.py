#!/usr/bin/env python3
"""Apply EmiWarp's integration points to an upstream Warp checkout.

Why this exists
---------------
A long-lived fork has to re-apply its changes on top of a moving upstream. The
two obvious approaches both fail:

  * `git merge -X ours` resolves *conflicting hunks* in our favour. The hunks
    most likely to conflict are exactly the ones upstream just changed — i.e.
    the fixes we want. It never breaks the build, because it quietly drops
    upstream work, including security fixes. It is the wrong tool here.

  * Context diffs (`git apply`) match on surrounding lines. Upstream reformats
    or edits a neighbouring line and the patch fails, even though the thing we
    anchor to is untouched.

So EmiWarp anchors on a *single distinctive line* per integration point and
transforms around it. Neighbouring code can change freely. Every injection is
idempotent (guarded by a marker) and independently verifiable, so `--verify`
answers "is this tree fully patched?" without rebuilding.

Exit codes: 0 success, 1 an injection could not be applied, 2 bad usage.
"""

from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass, field
from pathlib import Path

MARKER = "EmiWarp:"


@dataclass
class Injection:
    """One anchored edit."""

    ident: str
    path: str
    anchor: str  # must occur exactly once in the file
    mode: str  # replace | before | after
    payload: str
    why: str
    # If present in the file, the injection is already applied.
    guard: str = ""
    # Extra text appended to end of file on first application.
    append: str = ""
    optional: bool = False
    notes: list[str] = field(default_factory=list)

    def marker(self) -> str:
        return f"{MARKER} {self.ident}"


INJECTIONS: list[Injection] = [
    Injection(
        ident="workspace-boot",
        path="app/src/root_view.rs",
        anchor="        let auth_onboarding_state = if auth_state.is_logged_in() {",
        mode="replace",
        payload=(
            "        // EmiWarp: workspace-boot — render the terminal immediately.\n"
            "        // EmiWarp has no account server, so there is no login state to\n"
            "        // wait on and no onboarding splash to show.\n"
            "        let auth_onboarding_state = if auth_state.is_logged_in()\n"
            "            || emiwarp::skip_onboarding()\n"
            "        {"
        ),
        why="Boot straight into the terminal workspace.",
    ),
    Injection(
        ident="provider-env",
        path="app/src/ai/agent_sdk/driver.rs",
        anchor="        let resolved_env_vars = Arc::new(env_vars);",
        mode="before",
        payload=(
            "        // EmiWarp: provider-env — repoint the spawned agent CLI at the\n"
            "        // user's configured endpoint. Applied last so it wins over any\n"
            "        // upstream-derived value.\n"
            "        env_vars.extend(emiwarp::harness_env_overlay());\n"
        ),
        why="Make every local harness provider-agnostic via its own env contract.",
    ),
    Injection(
        ident="egress-guard",
        path="crates/http_client/src/lib.rs",
        anchor="    pub async fn execute(&self, request: Request) -> reqwest::Result<Response> {",
        mode="replace",
        payload=(
            "    pub async fn execute(&self, request: Request) -> reqwest::Result<Response> {\n"
            "        // EmiWarp: egress-guard — fail closed on Warp-operated hosts.\n"
            "        let request = emiwarp_block_vendor_egress(request);"
        ),
        append=(
            "\n"
            "/// EmiWarp: egress-guard — backstop for any call site the patch series has\n"
            "/// not caught.\n"
            "///\n"
            "/// EmiWarp does not use Warp-operated infrastructure, so a request bound for\n"
            "/// one is always a defect. `reqwest::Error` has no public constructor, so\n"
            "/// rather than change this function's signature (which would ripple through\n"
            "/// every caller and guarantee merge conflicts), the request is null-routed to\n"
            "/// a closed local port. It fails immediately with a real connection error,\n"
            "/// and the warning above it says why.\n"
            "fn emiwarp_block_vendor_egress(mut request: Request) -> Request {\n"
            "    let url = request.wrapped.url().clone();\n"
            "    if !emiwarp::egress_allowed(url.as_str()) {\n"
            "        log::warn!(\"EmiWarp blocked egress to Warp-operated host: {url}\");\n"
            "        if let Ok(sink) = reqwest::Url::parse(\"http://127.0.0.1:1/emiwarp-blocked\") {\n"
            "            *request.wrapped.url_mut() = sink;\n"
            "        }\n"
            "    }\n"
            "    request\n"
            "}\n"
        ),
        why="Nothing reaches Warp's servers, even through an unpatched call site.",
    ),
    Injection(
        ident="brand-cli-name",
        path="crates/warp_core/src/channel/mod.rs",
        anchor='            Channel::Oss => "warp-oss",',
        mode="replace",
        payload='            Channel::Oss => "emiwarp",',
        guard='Channel::Oss => "emiwarp"',
        why="Rebrand the CLI command name.",
        notes=["Occurs twice upstream (cli_command_name and Display); both are rebranded."],
    ),
    Injection(
        ident="brand-ctrl-name",
        path="crates/warp_core/src/channel/mod.rs",
        anchor='            Channel::Oss => "warpctrl-oss",',
        mode="replace",
        payload='            Channel::Oss => "emiwarpctrl",',
        guard='Channel::Oss => "emiwarpctrl"',
        why="Rebrand the control CLI command name.",
    ),
    Injection(
        ident="brand-app-id",
        path="crates/warp_core/src/channel/state.rs",
        anchor='        let app_id = AppId::new("dev", "warp", "WarpOss");',
        mode="replace",
        payload='        let app_id = AppId::new("dev", "emiwarp", "EmiWarp");',
        guard='AppId::new("dev", "emiwarp", "EmiWarp")',
        why="Rebrand the application identity, isolating EmiWarp state from Warp's.",
    ),
    Injection(
        ident="binary-target",
        path="app/Cargo.toml",
        anchor='warp-oss',
        mode="replace",
        payload='emiwarp',
        guard='name = "emiwarp"',
        why="Rename the OSS binary target to `emiwarp`.",
        notes=[
            "Renames rather than adds: two [[bin]] targets sharing one source "
            "makes cargo build the binary twice and warn about it.",
            "Rewrites every occurrence — the [[bin]] name, default-run, and the "
            "bundle metadata key all have to move together or the manifest "
            "fails to parse.",
        ],
    ),
    Injection(
        ident="cli-branding",
        path="crates/warp_cli/src/lib.rs",
        anchor="""    name = "oz",
    display_name = "Oz",
    about = r#"The orchestration platform for cloud agents

The Oz CLI is a tool for running, managing, and orchestrating coding agents at scale.
Use the CLI to:
* Launch and inspect cloud agents
* Schedule cloud agents to run in the future
* Manage the environments that cloud agents run in
* Upload secrets to Oz's secure storage"#""",
        mode="replace",
        payload='''    name = "emiwarp",
    display_name = "EmiWarp",
    about = r#"A terminal that runs agents on models you control

EmiWarp reaches models through agent CLIs already installed on this machine,
using your own endpoint and credentials. It does not contact Warp-operated
infrastructure, and has no account or subscription of its own.
Use the CLI to:
* Run a coding agent against a local or hosted model
* Point any OpenAI- or Anthropic-compatible endpoint at the terminal
* Inspect which agent CLIs and local model servers were discovered"#''',
        guard='display_name = "EmiWarp"',
        why="Rebrand the CLI name, --version output and --help text.",
        notes=[
            "The `about` text is a long literal and the likeliest anchor to "
            "drift; a failure here is cosmetic and safe to re-anchor."
        ],
    ),
    Injection(
        ident="run-script-bin",
        path="script/run",
        anchor='WARP_BIN_NAME="warp-oss"',
        mode="replace",
        payload='WARP_BIN_NAME="emiwarp"',
        guard='WARP_BIN_NAME="emiwarp"',
        why="Keep ./script/run working after the binary rename.",
    ),
]

# Crates whose manifests need an `emiwarp` dependency for the injections above.
MANIFEST_DEPS = [
    ("app/Cargo.toml", "emiwarp.workspace = true"),
    ("crates/http_client/Cargo.toml", "emiwarp.workspace = true"),
]


def unique_index(haystack: str, needle: str) -> int | None:
    first = haystack.find(needle)
    if first < 0:
        return None
    if haystack.find(needle, first + len(needle)) >= 0:
        return -1  # ambiguous
    return first


def apply_one(root: Path, inj: Injection, dry_run: bool) -> tuple[bool, str]:
    path = root / inj.path
    if not path.exists():
        return (inj.optional, f"missing file {inj.path}")

    text = path.read_text()
    guard = inj.guard or inj.marker()
    if guard in text:
        return (True, "already applied")

    # `brand-cli-name` intentionally matches two identical arms; rewrite all.
    occurrences = text.count(inj.anchor)
    if occurrences == 0:
        return (
            False,
            f"anchor not found in {inj.path}\n"
            f"        anchor: {inj.anchor.strip()!r}\n"
            f"        upstream likely moved or reworded this line; "
            f"re-anchor `{inj.ident}` in scripts/emiwarp/overlay.py",
        )

    if inj.mode == "replace":
        new_text = text.replace(inj.anchor, inj.payload)
    elif inj.mode == "before":
        if occurrences != 1:
            return (False, f"anchor for {inj.ident} occurs {occurrences}x; expected 1")
        new_text = text.replace(inj.anchor, inj.payload + inj.anchor, 1)
    elif inj.mode == "after":
        if occurrences != 1:
            return (False, f"anchor for {inj.ident} occurs {occurrences}x; expected 1")
        new_text = text.replace(inj.anchor, inj.anchor + inj.payload, 1)
    else:
        raise ValueError(f"unknown mode {inj.mode}")

    if inj.append:
        new_text = new_text.rstrip("\n") + "\n" + inj.append

    if not dry_run:
        path.write_text(new_text)
    return (True, f"applied ({occurrences} site{'s' if occurrences > 1 else ''})")


def ensure_manifest_dep(root: Path, rel: str, line: str, dry_run: bool) -> tuple[bool, str]:
    path = root / rel
    if not path.exists():
        return (False, f"missing manifest {rel}")
    text = path.read_text()
    if line in text:
        return (True, "already present")
    if "[dependencies]" not in text:
        return (False, f"no [dependencies] table in {rel}")
    new_text = text.replace("[dependencies]", f"[dependencies]\n{line}", 1)
    if not dry_run:
        path.write_text(new_text)
    return (True, "added")


def ensure_workspace_member(root: Path, dry_run: bool) -> tuple[bool, str]:
    path = root / "Cargo.toml"
    if not path.exists():
        return (False, "workspace Cargo.toml not found")
    text = path.read_text()
    if 'emiwarp = { path = "crates/emiwarp" }' in text:
        return (True, "already present")
    anchor = 'field_mask = { path = "crates/field_mask" }'
    if anchor not in text:
        return (False, "could not anchor emiwarp into [workspace.dependencies]")
    new_text = text.replace(
        anchor, 'emiwarp = { path = "crates/emiwarp" }\n' + anchor, 1
    )
    if not dry_run:
        path.write_text(new_text)
    return (True, "added")


def verify(root: Path) -> int:
    missing = []
    for inj in INJECTIONS:
        path = root / inj.path
        guard = inj.guard or inj.marker()
        if not path.exists() or guard not in path.read_text():
            missing.append(inj.ident)
    for rel, line in MANIFEST_DEPS:
        p = root / rel
        if not p.exists() or line not in p.read_text():
            missing.append(f"dep:{rel}")
    ws = root / "Cargo.toml"
    if not ws.exists() or 'emiwarp = { path = "crates/emiwarp" }' not in ws.read_text():
        missing.append("dep:workspace")

    if missing:
        print("FAIL overlay incomplete: " + ", ".join(missing), file=sys.stderr)
        return 1
    print(f"OK  overlay verified — {len(INJECTIONS)} injections, "
          f"{len(MANIFEST_DEPS) + 1} manifest edits")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", default=".", type=Path)
    ap.add_argument("--verify", action="store_true",
                    help="check the tree is fully patched; make no changes")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--list", action="store_true", help="describe every injection")
    args = ap.parse_args()

    root = args.root.resolve()

    # Everything below assumes `root` is a warp checkout. Say so plainly rather
    # than failing later with a stack trace on a missing file.
    if not args.list and not (root / "Cargo.toml").exists():
        print(
            f"error: {root} does not look like a warp checkout "
            f"(no Cargo.toml).\n"
            f"       In the overlay layout the build tree is created by "
            f"scripts/sync_and_build.sh on its first real run.",
            file=sys.stderr,
        )
        return 1

    if args.list:
        for inj in INJECTIONS:
            print(f"{inj.ident:<16} {inj.path}\n{'':<16} {inj.why}")
            for n in inj.notes:
                print(f"{'':<16} note: {n}")
        return 0

    if args.verify:
        return verify(root)

    failures = []

    ok, msg = ensure_workspace_member(root, args.dry_run)
    print(f"[{'ok' if ok else 'FAIL':>4}] workspace-dep      {msg}")
    if not ok:
        failures.append("workspace-dep")

    for rel, line in MANIFEST_DEPS:
        ok, msg = ensure_manifest_dep(root, rel, line, args.dry_run)
        print(f"[{'ok' if ok else 'FAIL':>4}] dep {rel:<30} {msg}")
        if not ok:
            failures.append(rel)

    for inj in INJECTIONS:
        ok, msg = apply_one(root, inj, args.dry_run)
        print(f"[{'ok' if ok else 'FAIL':>4}] {inj.ident:<18} {msg}")
        if not ok:
            failures.append(inj.ident)

    if failures:
        print(
            "\nOverlay incomplete. Unapplied: " + ", ".join(failures) +
            "\nUpstream moved an anchor. Fix the anchor in "
            "scripts/emiwarp/overlay.py, then re-run.\n"
            "Nothing was force-merged and no upstream change was discarded.",
            file=sys.stderr,
        )
        return 1

    print(f"\nOverlay complete — {len(INJECTIONS)} injections applied.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
