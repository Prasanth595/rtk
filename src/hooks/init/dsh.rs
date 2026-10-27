//! DeepSeek Harness (DSH) agent: hook install/uninstall helpers.
//!
//! DSH reads `AGENTS.md` (and `CLAUDE.md`) from the project root and, for
//! global installs, from `$DSH_HOME` (default `~/.dsh`). It has no
//! `PreToolUse` command hook, so RTK is configured through the `full`
//! awareness block written into `AGENTS.md`; the agent is taught to prefix
//! every shell command with `rtk` itself.

use super::*;
use crate::core::user_dirs;
use crate::hooks::constants::{DSH_DIR, DSH_HOME_ENV};
use std::ffi::OsString;

// ── path helpers ─────────────────────────────────────────────────────────────

fn resolve_dsh_home() -> Result<PathBuf> {
    resolve_dsh_home_from(user_dirs::home(), user_dirs::env_path(DSH_HOME_ENV))
}

fn resolve_dsh_home_from(home_dir: Option<PathBuf>, dsh_home: Option<OsString>) -> Result<PathBuf> {
    resolve_config_dir(
        dsh_home,
        home_dir,
        DSH_DIR,
        "Cannot determine DSH home directory. Set $DSH_HOME or $HOME.",
    )
}

// ── public entry points ───────────────────────────────────────────────────────

/// Returns true if the DSH `AGENTS.md` RTK instructions block is installed
/// (harness home first, then the project root).
pub fn is_configured() -> bool {
    let Ok(dsh_home) = resolve_dsh_home() else {
        return false;
    };
    let global_agents_md = dsh_home.join(AGENTS_MD);
    if global_agents_md.exists()
        && let Ok(content) = fs::read_to_string(&global_agents_md)
    {
        return content.contains(RTK_BLOCK_START);
    }
    // Also check the local project AGENTS.md.
    if let Ok(local) = user_dirs::current_dir() {
        let local_agents_md = local.join(AGENTS_MD);
        if local_agents_md.exists()
            && let Ok(content) = fs::read_to_string(&local_agents_md)
        {
            return content.contains(RTK_BLOCK_START);
        }
    }
    false
}

pub fn run_dsh_mode(global: bool, ctx: InitContext) -> Result<()> {
    if global {
        let dsh_home = resolve_dsh_home()?;
        run_dsh_mode_at(&dsh_home, global, ctx)
    } else {
        let base_dir = user_dirs::current_dir()?;
        run_dsh_mode_at(&base_dir, global, ctx)
    }
}

pub fn uninstall_dsh(global: bool, ctx: InitContext) -> Result<()> {
    let base_dir = if global {
        resolve_dsh_home()?
    } else {
        user_dirs::current_dir()?
    };
    let removed = uninstall_dsh_at(&base_dir, ctx)?;

    if removed.is_empty() {
        println!("RTK DSH support was not installed (nothing to remove)");
    } else {
        let header = if ctx.dry_run {
            "[dry-run] would uninstall RTK for DeepSeek Harness:"
        } else {
            "RTK uninstalled for DeepSeek Harness:"
        };
        println!("{header}");
        for item in &removed {
            println!("  - {item}");
        }
    }
    if ctx.dry_run {
        print_dry_run_footer();
    }
    Ok(())
}

// ── internal helpers ──────────────────────────────────────────────────────────

fn run_dsh_mode_at(base_dir: &Path, global: bool, ctx: InitContext) -> Result<()> {
    let agents_md_path = base_dir.join(AGENTS_MD);

    write_rtk_block(
        &agents_md_path,
        &rtk_block(RTK_AWARENESS_FULL),
        "RTK instructions",
        if global {
            "rtk init -g --agent dsh"
        } else {
            "rtk init --agent dsh"
        },
        ctx,
    )?;

    print_instructions_agents_awareness_note("DeepSeek Harness", ctx);
    if !ctx.dry_run {
        println!("\nRTK configured for DeepSeek Harness (DSH).\n");
        println!("  AGENTS.md: {}", agents_md_path.display());
        println!("  DSH will now use rtk commands for token savings.");
        println!("  Test with: git status\n");
    }
    Ok(())
}

fn uninstall_dsh_at(base_dir: &Path, ctx: InitContext) -> Result<Vec<String>> {
    let InitContext {
        verbose, dry_run, ..
    } = ctx;
    let agents_md_path = base_dir.join(AGENTS_MD);
    let mut removed = Vec::new();

    if agents_md_path.exists() {
        let content = fs::read_to_string(&agents_md_path)
            .with_context(|| format!("Failed to read AGENTS.md: {}", agents_md_path.display()))?;

        if content.contains(RTK_BLOCK_START) {
            let (cleaned, did_remove) = remove_rtk_block(&content);
            if did_remove {
                if dry_run {
                    println!(
                        "[dry-run] would remove RTK instructions block from {}",
                        agents_md_path.display()
                    );
                } else {
                    atomic_write(&agents_md_path, &cleaned).with_context(|| {
                        format!("Failed to write AGENTS.md: {}", agents_md_path.display())
                    })?;
                    if verbose > 0 {
                        eprintln!("Updated AGENTS.md: {}", agents_md_path.display());
                    }
                }
                removed.push(format!(
                    "AGENTS.md: removed rtk-instructions block ({})",
                    agents_md_path.display()
                ));
            }
        }
    }

    Ok(removed)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_dsh_mode_creates_agents_md_with_rtk_block() {
        let temp = TempDir::new().unwrap();
        run_dsh_mode_at(temp.path(), false, InitContext::default()).unwrap();

        let agents_md = temp.path().join("AGENTS.md");
        assert!(agents_md.exists(), "AGENTS.md should be created");
        let content = fs::read_to_string(&agents_md).unwrap();
        assert!(
            content.contains(RTK_BLOCK_START),
            "AGENTS.md should contain the RTK instructions block"
        );
        assert!(
            content.contains(RTK_AWARENESS_FULL),
            "AGENTS.md should contain full awareness text"
        );
    }

    #[test]
    fn test_dsh_mode_is_idempotent() {
        let temp = TempDir::new().unwrap();
        run_dsh_mode_at(temp.path(), false, InitContext::default()).unwrap();
        let first = fs::read_to_string(temp.path().join("AGENTS.md")).unwrap();

        run_dsh_mode_at(temp.path(), false, InitContext::default()).unwrap();
        let second = fs::read_to_string(temp.path().join("AGENTS.md")).unwrap();

        assert_eq!(first, second, "second run must be a no-op");
    }

    #[test]
    fn test_dsh_mode_preserves_existing_user_content() {
        let temp = TempDir::new().unwrap();
        let agents_md = temp.path().join("AGENTS.md");
        fs::write(&agents_md, "# Team rules\n\nDo not break prod.\n").unwrap();

        run_dsh_mode_at(temp.path(), false, InitContext::default()).unwrap();

        let content = fs::read_to_string(&agents_md).unwrap();
        assert!(content.contains("# Team rules"), "user content preserved");
        assert!(
            content.contains("Do not break prod."),
            "user content preserved"
        );
        assert!(content.contains(RTK_BLOCK_START), "RTK block appended");
    }

    #[test]
    fn test_dsh_mode_dry_run_writes_nothing() {
        let temp = TempDir::new().unwrap();
        run_dsh_mode_at(
            temp.path(),
            false,
            InitContext {
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            !temp.path().join("AGENTS.md").exists(),
            "dry-run must not create AGENTS.md"
        );
    }

    #[test]
    fn test_uninstall_dsh_removes_rtk_block() {
        let temp = TempDir::new().unwrap();
        run_dsh_mode_at(temp.path(), false, InitContext::default()).unwrap();
        assert!(temp.path().join("AGENTS.md").exists());

        let removed = uninstall_dsh_at(temp.path(), InitContext::default()).unwrap();
        assert_eq!(removed.len(), 1, "one item removed");

        let content = fs::read_to_string(temp.path().join("AGENTS.md")).unwrap();
        assert!(
            !content.contains(RTK_BLOCK_START),
            "RTK block must be removed"
        );
    }

    #[test]
    fn test_uninstall_dsh_preserves_user_content() {
        let temp = TempDir::new().unwrap();
        let agents_md = temp.path().join("AGENTS.md");
        fs::write(&agents_md, "# Team rules\n\nDo not break prod.\n").unwrap();

        run_dsh_mode_at(temp.path(), false, InitContext::default()).unwrap();
        uninstall_dsh_at(temp.path(), InitContext::default()).unwrap();

        let content = fs::read_to_string(&agents_md).unwrap();
        assert!(
            content.contains("# Team rules"),
            "user content preserved after uninstall"
        );
        assert!(!content.contains(RTK_BLOCK_START), "RTK block removed");
    }

    #[test]
    fn test_uninstall_dsh_is_idempotent() {
        let temp = TempDir::new().unwrap();
        run_dsh_mode_at(temp.path(), false, InitContext::default()).unwrap();

        let first = uninstall_dsh_at(temp.path(), InitContext::default()).unwrap();
        let second = uninstall_dsh_at(temp.path(), InitContext::default()).unwrap();

        assert_eq!(first.len(), 1);
        assert!(second.is_empty(), "second uninstall should be a no-op");
    }

    #[test]
    fn test_uninstall_dsh_missing_file_is_ok() {
        let temp = TempDir::new().unwrap();
        let removed = uninstall_dsh_at(temp.path(), InitContext::default()).unwrap();
        assert!(removed.is_empty());
    }

    #[test]
    fn test_uninstall_dsh_dry_run_writes_nothing() {
        let temp = TempDir::new().unwrap();
        run_dsh_mode_at(temp.path(), false, InitContext::default()).unwrap();
        let before = fs::read_to_string(temp.path().join("AGENTS.md")).unwrap();

        uninstall_dsh_at(
            temp.path(),
            InitContext {
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();

        let after = fs::read_to_string(temp.path().join("AGENTS.md")).unwrap();
        assert_eq!(before, after, "dry-run must not modify AGENTS.md");
    }

    #[test]
    fn test_resolve_dsh_home_prefers_dsh_home_env() {
        use std::ffi::OsString;
        let custom = OsString::from("/tmp/my-dsh-home");
        let home = PathBuf::from("/tmp/home");

        let result = resolve_dsh_home_from(Some(home.clone()), Some(custom.clone())).unwrap();
        assert_eq!(result, PathBuf::from(custom));
    }

    #[test]
    fn test_resolve_dsh_home_falls_back_to_home_dot_dsh() {
        use std::ffi::OsString;
        let home = PathBuf::from("/tmp/home");

        let from_empty = resolve_dsh_home_from(Some(home.clone()), Some(OsString::new())).unwrap();
        let from_missing = resolve_dsh_home_from(Some(home.clone()), None).unwrap();

        assert_eq!(from_empty, home.join(".dsh"));
        assert_eq!(from_missing, home.join(".dsh"));
    }

    #[test]
    fn test_is_configured_detects_local_agents_md() {
        use crate::core::test_isolation;
        let temp = test_isolation::tempdir();
        let _entered = test_isolation::enter(temp.path());

        assert!(!is_configured(), "not configured yet");

        run_dsh_mode_at(temp.path(), false, InitContext::default()).unwrap();
        assert!(is_configured(), "should be configured after install");
    }

    #[test]
    fn test_dsh_always_receives_full_awareness() {
        for level in [
            AwarenessLevel::Default,
            AwarenessLevel::High,
            AwarenessLevel::Full,
        ] {
            let temp = TempDir::new().unwrap();
            run_dsh_mode_at(
                temp.path(),
                false,
                InitContext {
                    awareness: level,
                    ..Default::default()
                },
            )
            .unwrap();
            let content = fs::read_to_string(temp.path().join("AGENTS.md")).unwrap();
            assert!(
                content.contains(RTK_AWARENESS_FULL),
                "dsh must always get full awareness, level={level}"
            );
        }
    }
}
