#!/usr/bin/env python3
"""Render the spec-autobump PR body markdown.

Kept out of the workflow's inline shell so the markdown (full of backticks and
`${...}` examples) isn't fighting shell quoting — and so the body is easy to
eyeball/diff. Driven by `.github/workflows/spec-autobump.yml` (ENG-3563).

Reads the captured oasdiff breaking output from a file so the verbatim verdict
lands in the PR. Writes the rendered markdown to stdout.

Usage:
  render_autobump_pr_body.py --new-tag vX.Y.Z --old-tag vA.B.C \
      --verdict {non-breaking|breaking} --oasdiff-file PATH
"""
import argparse
import sys


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--new-tag", required=True)
    ap.add_argument("--old-tag", required=True)
    ap.add_argument("--verdict", required=True, choices=["non-breaking", "breaking"])
    ap.add_argument("--oasdiff-file", required=True)
    args = ap.parse_args()

    try:
        with open(args.oasdiff_file) as f:
            oasdiff_out = f.read().strip() or "(no output captured)"
    except OSError:
        oasdiff_out = "(no output captured)"

    out = []
    out.append(
        f"nexus-exchange-api released **{args.new_tag}** "
        f"(was pinned at **{args.old_tag}**). Opened automatically by "
        f"`spec-autobump` (ENG-3563).\n"
    )
    out.append(f"### oasdiff verdict: **{args.verdict}**\n")
    out.append(
        f"Classified `{args.old_tag} -> {args.new_tag}` with "
        f"`oasdiff breaking --fail-on ERR` (the same gate the api repo runs as "
        f'"Classify API changes"). ERR-level changes are breaking; WARN/INFO are '
        f"not.\n"
    )
    out.append("<details><summary>oasdiff breaking output</summary>\n")
    out.append(f"```\n{oasdiff_out}\n```\n")
    out.append("</details>\n")
    out.append("### Applied\n")
    out.append(f"- Bumped `.api-version` to `{args.new_tag}`.")
    out.append(
        '- Updated the bot-managed "currently targets" line + the README '
        "compat-table row.\n"
    )

    if args.verdict == "non-breaking":
        out.append("### Merge gating (non-breaking)\n")
        out.append(
            "GitHub auto-merge has been **armed** (squash). It does NOT merge on "
            "its own — the PR can only merge once:\n"
        )
        out.append(
            "- the required status checks pass: the SDK `drift` check "
            "(code <-> endpoints <-> pinned spec) and CI `test`. An additive spec "
            "change needs no SDK code edits, so these stay green; they fail only "
            "if an *implemented* op was removed/renamed (which oasdiff would have "
            "classified as breaking)."
        )
        out.append(
            "- the **ENG-4149 ruleset bypass** for this bot is configured to "
            "satisfy the 1-review + code-owner-review rule for pin-bump PRs only.\n"
        )
        out.append(
            "Until ENG-4149 lands, this PR sits green awaiting the bypass — "
            "auto-merge cannot fire. No premature merge."
        )
    else:
        out.append("### Merge gating (breaking)\n")
        out.append(
            f"oasdiff flagged an ERR-level (breaking) change, so auto-merge was "
            f"**NOT** armed. A human owns this: review what `{args.new_tag}` "
            f"changes, make the SDK code changes it implies, plan the SDK version "
            f"bump, then merge. Labeled `breaking · needs-SDK-update`."
        )

    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
