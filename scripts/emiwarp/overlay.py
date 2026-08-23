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
* Inspect which agent CLIs and local model servers were discovered"#,
    long_about = r#"EmiWarp — a terminal that runs agents on models you control

EmiWarp reaches models through agent CLIs already installed on this machine
(claude, codex, gemini, opencode, qwen), pointed at an endpoint you configure.
Credentials stay with the CLI that owns them; EmiWarp stores none of its own.

It does not contact Warp-operated infrastructure and has no account, tier or
subscription. Subcommands that drive Warp's cloud agent platform remain in the
parser but cannot reach it — requests to Warp-operated hosts fail closed."#''',
        guard='display_name = "EmiWarp"',
        why="Rebrand the CLI name, --version output and --help text.",
        notes=[
            "clap prefers the struct's doc comment over `about` for long help, "
            "so `long_about` is set explicitly rather than editing the doc "
            "comment — a smaller, more stable anchor.",
            "The text is a long literal and the likeliest anchor to drift; a "
            "failure here is cosmetic and safe to re-anchor.",
        ],
    ),
    Injection(
        ident="bundle-branding",
        path="script/macos/bundle",
        anchor='    WARP_BIN="warp-oss"\n    BUNDLE_ID="dev.warp.WarpOss"\n    WARP_APP_NAME="WarpOss"\n    WARP_SCHEME_NAME="warposs"',
        mode="replace",
        payload='    WARP_BIN="emiwarp"\n    BUNDLE_ID="dev.emiwarp.EmiWarp"\n    WARP_APP_NAME="EmiWarp"\n    WARP_SCHEME_NAME="emiwarp"',
        guard='WARP_APP_NAME="EmiWarp"',
        why="Produce an installable EmiWarp.app rather than WarpOss.app.",
        notes=[
            "Without this the bundler looks for a `warp-oss` binary that "
            "binary-target already renamed, so bundling fails outright.",
            "BUNDLE_ID matches brand-app-id so macOS treats EmiWarp as a "
            "separate app from any installed Warp.",
        ],
    ),
    Injection(
        ident="bundle-metadata",
        path="app/Cargo.toml",
        anchor='category = "public.app-category.developer-tools"\ncopyright = "© 2025, Denver Technologies, Inc"\nidentifier = "dev.warp.WarpOss"\nname = "WarpOss"\nresources = ["assets/onboarding"]\nicon = ["channels/oss/icon/no-padding/512x512.png", "channels/oss/icon/no-padding/icon.ico"]\nshort_description = "The open-source, cloud-backed terminal for individuals and teams."',
        mode="replace",
        payload='category = "public.app-category.developer-tools"\ncopyright = "© 2025, Denver Technologies, Inc"\nidentifier = "dev.emiwarp.EmiWarp"\nname = "EmiWarp"\nresources = ["assets/onboarding"]\nicon = ["channels/oss/icon/no-padding/512x512.png", "channels/oss/icon/no-padding/icon.ico"]\nshort_description = "A terminal that runs agents on models you control."',
        guard='name = "EmiWarp"',
        why="Name the produced .app bundle EmiWarp, not WarpOss.",
        notes=[
            "cargo-bundle reads the app name and identifier from this "
            "metadata block, not from script/macos/bundle's variables, so "
            "bundle-branding alone still emits WarpOss.app.",
            "Upstream copyright is left intact: the code is still theirs.",
        ],
    ),
    Injection(
        ident="menu-account-items",
        path="app/src/workspace/view.rs",
        anchor='        if self.auth_state.is_anonymous_or_logged_out() {\n            items.push(\n                MenuItemFields::new("Sign up")\n                    .with_on_select_action(WorkspaceAction::SignupAnonymousUser)\n                    .into_item(),\n            );\n        }\n\n        // Check if the user is on any paid plan to determine whether to show "Billing and Usage" or "Upgrade"\n        let is_on_paid_plan = UserWorkspaces::as_ref(app)\n            .current_workspace()\n            .map(|workspace| workspace.billing_metadata.is_user_on_paid_plan())\n            .unwrap_or(false);\n\n        if is_on_paid_plan {\n            items.push(\n                MenuItemFields::new("Billing and usage")\n                    .with_on_select_action(WorkspaceAction::ShowSettingsPage(\n                        SettingsSection::BillingAndUsage,\n                    ))\n                    .into_item(),\n            );\n        } else {\n            items.push(\n                MenuItemFields::new("Upgrade")\n                    .with_on_select_action(WorkspaceAction::ShowUpgrade)\n                    .into_item(),\n            );\n        }\n\n        items.push(\n            MenuItemFields::new("Invite a friend")\n                .with_on_select_action(WorkspaceAction::ShowReferralSettingsPage)\n                .into_item(),\n        );\n\n        if !self.auth_state.is_anonymous_or_logged_out() {\n            items.push(\n                MenuItemFields::new("Log out")\n                    .with_on_select_action(WorkspaceAction::LogOut)\n                    .into_item(),\n            );\n        }\n',
        mode="replace",
        payload='        // EmiWarp: the account and billing menu items are removed, not\n        // disabled. There is no account server and no plans, so Sign up,\n        // Upgrade, Billing and usage, Invite a friend and Log out all\n        // describe things this build does not have.\n',
        guard="EmiWarp: the account and billing menu items are removed",
        why="Drop Sign up / Upgrade / Billing / Invite / Log out from the menu.",
        notes=[
            "The egress guard stops the network calls, but the menu still "
            "advertised them. A paywall you cannot reach is still a paywall "
            "in the UI.",
        ],
    ),
    Injection(
        ident="settings-nav",
        path="app/src/settings_view/mod.rs",
        anchor='        if FeatureFlag::WarpControlCli.is_enabled() {\n            let shared_blocks_index = nav_items',
        mode="replace",
        payload='        // EmiWarp: drop the sections that only describe Warp-operated\n        // services. Nothing in this build reaches a server, so an account\n        // page, a billing page, referral credits, cloud teams, shared\n        // blocks and Drive sync would be inert UI for features it does not\n        // have. Filtering the built list rather than editing the literal\n        // keeps this to one anchored hunk as upstream churns the vec.\n        nav_items.retain(|item| match item {\n            SettingsNavItem::Page(section) => !matches!(\n                section,\n                SettingsSection::Account\n                    | SettingsSection::BillingAndUsage\n                    | SettingsSection::Referrals\n                    | SettingsSection::Teams\n                    | SettingsSection::SharedBlocks\n                    | SettingsSection::WarpDrive\n            ),\n            SettingsNavItem::Umbrella(umbrella) => umbrella.label != "Cloud platform",\n        });\n\n        if FeatureFlag::WarpControlCli.is_enabled() {\n            let shared_blocks_index = nav_items',
        guard="EmiWarp: drop the sections that only describe",
        why="Remove account, billing, referrals, teams and cloud settings pages.",
    ),
    Injection(
        ident="settings-default-page",
        path="app/src/settings_view/mod.rs",
        anchor="            other => other.unwrap_or_default(),",
        mode="replace",
        payload="            // EmiWarp: Account is upstream's default and settings-nav\n            // removes it, so fall back to a page that still exists.\n            other => other.unwrap_or(SettingsSection::Appearance),",
        guard="EmiWarp: Account is upstream's default",
        why="Open Settings on Appearance now that Account is gone.",
    ),
    Injection(
        ident="auth-local-identity",
        path="crates/warp_server_auth/src/auth_state.rs",
        anchor='    pub fn is_logged_in(&self) -> bool {\n        self.credentials.read().is_some()\n    }',
        mode="replace",
        payload='    pub fn is_logged_in(&self) -> bool {\n        // EmiWarp: there is no account server to be logged out of. This\n        // answers the render and feature gates, which ask it to mean "does\n        // this client have a usable identity" — it does, a local one.\n        // Nothing that would contact Warp reads this: egress to\n        // Warp-operated hosts is refused in http_client regardless.\n        true\n    }',
        guard="EmiWarp: there is no account server to be logged out of",
        why="Give the client a local identity so nothing is account-gated.",
    ),
    Injection(
        ident="auth-not-anonymous",
        path="crates/warp_server_auth/src/auth_state.rs",
        anchor='    pub fn is_anonymous_or_logged_out(&self) -> bool {\n        !self.is_logged_in() || self.is_user_anonymous().unwrap_or(true)\n    }',
        mode="replace",
        payload='    pub fn is_anonymous_or_logged_out(&self) -> bool {\n        // EmiWarp: this gates every AI surface in the app. Upstream means\n        // "lacks a paid-capable Warp account"; EmiWarp has no accounts at\n        // all and runs inference on locally installed CLI agents, so the\n        // question does not apply and must not disable the UI.\n        false\n    }',
        guard="EmiWarp: this gates every AI surface in the app",
        why="Ungate AI features that upstream hides behind an account.",
        notes=[
            "This is the single gate behind \"To use AI features, please "
            "create an account\" and every disabled AI toggle.",
        ],
    ),
    Injection(
        ident="local-harnesses",
        path="app/src/ai/harness_availability.rs",
        anchor='fn default_harnesses() -> Vec<HarnessAvailability> {\n    vec![HarnessAvailability {\n        harness: Harness::Oz,\n        display_name: harness_display::display_name(Harness::Oz).to_string(),\n        enabled: true,\n        available_models: vec![],\n    }]\n}',
        mode="replace",
        payload='fn default_harnesses() -> Vec<HarnessAvailability> {\n    // EmiWarp: upstream calls this the fallback "before the server responds".\n    // EmiWarp never contacts that server, so this list is permanent, not a\n    // fallback — and returning Oz alone would offer the one harness that\n    // structurally cannot run here, which is why every AI surface looked dead.\n    //\n    // Instead offer the agent CLIs actually installed on this machine. Each\n    // carries its own login, so nothing here needs an EmiWarp account.\n    fn availability(harness: Harness) -> HarnessAvailability {\n        HarnessAvailability {\n            harness,\n            display_name: harness_display::display_name(harness).to_string(),\n            enabled: true,\n            available_models: vec![],\n        }\n    }\n\n    let mut out: Vec<HarnessAvailability> = emiwarp::discover()\n        .usable_harnesses()\n        .filter_map(|found| match found.id.as_str() {\n            "claude-code" => Some(Harness::Claude),\n            "codex" => Some(Harness::Codex),\n            "gemini" => Some(Harness::Gemini),\n            "opencode" => Some(Harness::OpenCode),\n            _ => None,\n        })\n        .map(availability)\n        .collect();\n\n    // Nothing installed yet: still offer the picker so the setup prompt can\n    // say what to install, rather than presenting an empty menu.\n    if out.is_empty() {\n        out.push(availability(Harness::Claude));\n    }\n    out\n}',
        guard="EmiWarp: upstream calls this the fallback",
        why="Offer the locally installed agent CLIs instead of Oz.",
        notes=[
            "Upstream treats this as a pre-fetch placeholder the server "
            "replaces. With egress severed the server never answers, so "
            "whatever this returns is what the user gets, forever.",
        ],
    ),
    Injection(
        ident="brand-agent-name",
        path="app/src/ai/harness_display.rs",
        anchor='        Harness::Oz => "Warp Agent",',
        mode="replace",
        payload='        Harness::Oz => "EmiWarp Agent",',
        guard='Harness::Oz => "EmiWarp Agent"',
        why="Rebrand the built-in agent name.",
    ),
    Injection(
        ident="brand-logs-menu",
        path="app/src/workspace/view.rs",
        anchor='MenuItemFields::new("View Warp logs")',
        mode="replace",
        payload='MenuItemFields::new("View EmiWarp logs")',
        guard='MenuItemFields::new("View EmiWarp logs")',
        why="Rebrand the logs menu item.",
    ),
    Injection(
        ident="brand-logs-cmd",
        path="app/src/workspace/mod.rs",
        anchor='            "View Warp logs",',
        mode="replace",
        payload='            "View EmiWarp logs",',
        guard='"View EmiWarp logs"',
        why="Rebrand the logs command-palette entry.",
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

# EmiWarp-owned files copied over upstream paths. Binary assets cannot be
# expressed as anchored text edits, and the destinations belong to upstream, so
# a sync would otherwise restore Warp's own icon over ours.
ASSET_COPIES = [
    ("assets/branding/512x512.png", "app/channels/oss/icon/no-padding/512x512.png"),
    ("assets/branding/icon.ico", "app/channels/oss/icon/no-padding/icon.ico"),
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


def copy_assets(root: Path, dry_run: bool) -> list[tuple[bool, str]]:
    import filecmp
    import shutil

    out = []
    for src_rel, dst_rel in ASSET_COPIES:
        src, dst = root / src_rel, root / dst_rel
        if not src.exists():
            out.append((False, f"missing source {src_rel}"))
            continue
        if dst.exists() and filecmp.cmp(src, dst, shallow=False):
            out.append((True, "already in place"))
            continue
        if not dry_run:
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)
        out.append((True, f"copied -> {dst_rel}"))
    return out


def verify(root: Path) -> int:
    import filecmp

    missing = []
    for src_rel, dst_rel in ASSET_COPIES:
        src, dst = root / src_rel, root / dst_rel
        if not src.exists() or not dst.exists() or not filecmp.cmp(src, dst, shallow=False):
            missing.append(f"asset:{dst_rel}")
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

    for (ok, msg), (_, dst_rel) in zip(copy_assets(root, args.dry_run), ASSET_COPIES):
        print(f"[{'ok' if ok else 'FAIL':>4}] asset {Path(dst_rel).name:<12} {msg}")
        if not ok:
            failures.append(f"asset:{dst_rel}")

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
