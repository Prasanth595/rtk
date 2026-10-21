# RTK (Fork with Vercel CLI Token Optimizer)

> **Forked `rtk` to include Vercel CLI optimization, reducing command output token usage by 61.3%.**

This fork extends upstream [`rtk-ai/rtk`](https://github.com/rtk-ai/rtk) with native token compression and auto-rewrite filters for the **Vercel CLI** (`vercel` and `npx vercel`).

---

> [!IMPORTANT]
> ### ⚡ Agent One-Shot Installation Prompts
>
> Paste the appropriate prompt directly into your AI coding assistant (Claude Code, Gemini CLI, Cursor, Antigravity, OpenCode):
>
> #### Option A: If you already have `rtk` installed (Add Vercel Optimizer)
> ```bash
> Run this shell command to install and trust rpatty-dev's Vercel CLI optimizer in your active RTK setup:
> curl -fsSL https://raw.githubusercontent.com/rpatty-dev/rtk/feat/vercel-cli-filter/src/filters/vercel.toml > /tmp/vercel_filter.toml && (TARGET_DIR="$([ "$(uname)" = "Darwin" ] && echo "$HOME/Library/Application Support/rtk" || echo "$HOME/.config/rtk")" && mkdir -p "$TARGET_DIR" && (grep -q "\[filters.vercel\]" "$TARGET_DIR/filters.toml" 2>/dev/null || cat /tmp/vercel_filter.toml >> "$TARGET_DIR/filters.toml") && printf "y\n" | rtk trust 2>/dev/null || true) && echo "✓ Vercel CLI optimizer active for RTK"
> ```
>
> #### Option B: If you do NOT have `rtk` installed (Fresh Full Install + Vercel Optimizer)
> ```bash
> Run this shell command to install RTK and activate rpatty-dev's Vercel CLI optimizer in one step:
> curl -fsSL https://raw.githubusercontent.com/rtk-ai/rtk/refs/heads/master/install.sh | sh && TARGET_DIR="$([ "$(uname)" = "Darwin" ] && echo "$HOME/Library/Application Support/rtk" || echo "$HOME/.config/rtk")" && mkdir -p "$TARGET_DIR" && curl -fsSL https://raw.githubusercontent.com/rpatty-dev/rtk/feat/vercel-cli-filter/src/filters/vercel.toml > "$TARGET_DIR/filters.toml" && printf "y\n" | rtk trust 2>/dev/null || true && echo "✓ RTK + Vercel CLI optimizer fully installed"
> ```

---

## What is Different in This Fork?

1. **Vercel CLI Filter (`src/filters/vercel.toml`)**:
   - Automatically intercepts `vercel` and `npx vercel` commands across LLM agent hooks (Gemini CLI, Claude Code, Cursor, Codex, OpenCode).
   - Strips verbose post-deployment hints (`Next steps:`, `vercel git connect`, `vercel curl`, `vercel inspect`, `vercel redeploy`).
   - Removes deployment protection disclaimers, duplicate progress loops (`Uploading`, `Deploying`, `Last progress:`).
   - Preserves critical deployment URLs, alias targets (`▲ Aliased`), and real error messages.

2. **Automated Discovery Rule (`src/discover/rules.rs`)**:
   - Registers `vercel` in the command interception engine under the `Cloud` category.

3. **Inline Test Suite**:
   - Embedded unit tests for `src/filters/vercel.toml` verifying deployment output compacting and status command passthrough.

---

## Benchmark & Proof

| Metric | Raw `vercel` Output | `rtk vercel` Filtered | Reduction |
| :--- | :--- | :--- | :--- |
| **Output Size** | 860 bytes | 333 bytes | **-61.3%** |
| **Estimated LLM Tokens** | ~215 tokens | ~83 tokens | **-61.3%** |

### Before (Raw Vercel Output — 860 bytes):
```text
Vercel CLI 65.0.0
  Inspect         https://vercel.com/rpattys-projects/orchardkitchensni/6Nfz5mUzyHBRTET4MP6XaPTa6x1X
  Production      https://orchardkitchensni-1izpygdbd-rpattys-projects.vercel.app
▲ Aliased         https://orchardkitchensni.co.uk

✓ Ready in 12s

! Deployment Protection is on for this project (Vercel Authentication). Use `vercel curl` to access it.

Next steps:
- Automatically deploy changes on every push by connecting Git:
  vercel git connect
- Check the production response:
  vercel curl https://orchardkitchensni.co.uk
- View build logs:
  vercel inspect orchardkitchensni-1izpygdbd-rpattys-projects.vercel.app --logs
- Create a new deployment from the same source:
  vercel redeploy orchardkitchensni-1izpygdbd-rpattys-projects.vercel.app
https://orchardkitchensni-1izpygdbd-rpattys-projects.vercel.app

Last progress: 0s ago
```

### After (`rtk vercel` Filtered Output — 333 bytes):
```text
Vercel CLI 65.0.0
  Inspect         https://vercel.com/rpattys-projects/orchardkitchensni/6Nfz5mUzyHBRTET4MP6XaPTa6x1X
  Production      https://orchardkitchensni-1izpygdbd-rpattys-projects.vercel.app
▲ Aliased         https://orchardkitchensni.co.uk
✓ Ready in 12s
https://orchardkitchensni-1izpygdbd-rpattys-projects.vercel.app
```

---

## Upstream Contribution
This optimization has also been submitted as a clean PR to the official RTK repository:
- **Upstream PR**: [rtk-ai/rtk#4723](https://github.com/rtk-ai/rtk/pull/4723)
