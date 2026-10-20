fn main() {
    // Reset SIGPIPE to default handler so writing to a closed pipe
    // e.g `rtk git log | head` exits silently instead of panicking.
    // Rust ignores SIGPIPE by default and with panic="abort" in the
    // release profile that becomes SIGABRT + coredump.
    #[cfg(unix)]
    #[allow(unsafe_code)]
    // nosemgrep: unsafe-block
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let code = match run_cli() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rtk: {:#}", e);
            1
        }
    };
    std::process::exit(code);
}

#[allow(clippy::too_many_arguments)]
fn uninstall_init_dispatch<UninstallHermes, UninstallStandard>(
    agent: Option<AgentTarget>,
    global: bool,
    gemini: bool,
    codex: bool,
    patch_mode: hooks::init::PatchMode,
    ctx: hooks::init::InitContext,
    uninstall_hermes: UninstallHermes,
    uninstall_standard: UninstallStandard,
) -> Result<()>
where
    UninstallHermes: FnOnce(hooks::init::InitContext) -> Result<()>,
    UninstallStandard: FnOnce(
        bool,
        bool,
        bool,
        bool,
        bool,
        bool,
        hooks::init::PatchMode,
        hooks::init::InitContext,
    ) -> Result<()>,
{
    if agent == Some(AgentTarget::Hermes) {
        uninstall_hermes(ctx)
    } else if agent == Some(AgentTarget::Trae) {
        hooks::init::uninstall_trae_mode(global, ctx)
    } else if agent == Some(AgentTarget::Antigravity) {
        hooks::init::uninstall_antigravity_mode(global, ctx)
    } else if agent == Some(AgentTarget::Droid) {
        hooks::init::uninstall_droid(global, ctx)
    } else if agent == Some(AgentTarget::Vibe) {
        hooks::init::uninstall_vibe(ctx)
    } else {
        let cursor = agent == Some(AgentTarget::Cursor);
        let pi = agent == Some(AgentTarget::Pi);
        let omp = agent == Some(AgentTarget::Omp);
        uninstall_standard(global, gemini, codex, cursor, pi, omp, patch_mode, ctx)
    }
}

fn is_native_test_expression(command: &[String]) -> bool {
    match command.first().map(String::as_str) {
        // `!` and `(` are shell syntax too, so they only mark a native
        // expression when what they apply to is one.
        Some("!") | Some("(") => is_native_test_expression(&command[1..]),
        Some(arg) => arg.starts_with('-'),
        None => false,
    }
}

/// Strip the shell grouping a command carries, returning how many negations were
/// removed along with the command itself.
///
/// `!` and `( … )` are both `test`'s syntax and the shell's, and
/// [`is_native_test_expression`] already treats them as possible native
/// prefixes. A command behind them is not a native expression, so it runs
/// through the test filter — and the negation and grouping the joined `sh -c`
/// string used to get from the shell have to come from somewhere.
///
/// They nest, so they are stripped in one loop until neither applies: a single
/// pass of each leaves `! ( cmd )` with `(` as its program, which is not found
/// — and the negation would then turn that 127 into a reported *pass* for a
/// command that never ran.
fn split_leading_negations(command: Vec<String>) -> (usize, Vec<String>) {
    let mut command = command;
    let mut negations = 0;

    loop {
        let bangs = command.iter().take_while(|token| *token == "!").count();
        // All `!` and nothing to negate: leave it alone so the runner reports it.
        if bangs > 0 && bangs < command.len() {
            negations += bangs;
            command = command[bangs..].to_vec();
            continue;
        }
        if let Some(grouped) = strip_outer_group(&command) {
            command = grouped;
            continue;
        }
        return (negations, command);
    }
}

/// Peel one `( … )` that wraps the whole command.
///
/// The parentheses must balance across the command, so `( a ) b ( c )` — where
/// the first `(` closes before the end — keeps both of them.
fn strip_outer_group(command: &[String]) -> Option<Vec<String>> {
    if command.len() <= 2 || command.first()? != "(" || command.last()? != ")" {
        return None;
    }

    let mut depth = 0usize;
    for (index, token) in command.iter().enumerate() {
        match token.as_str() {
            "(" => depth += 1,
            ")" => depth = depth.checked_sub(1)?,
            _ => {}
        }
        if depth == 0 && index + 1 < command.len() {
            return None;
        }
    }

    (depth == 0).then(|| command[1..command.len() - 1].to_vec())
}

/// `--shell` runs one complete script, so it takes exactly one positional.
///
/// Clap cannot express "exactly one value for this positional when that flag is
/// present", and `run` spells the same rule as `conflicts_with`/`requires`
/// around its own `-c`. Enforcing it here keeps the other three surfaces
/// answering with a usage error instead of failing mid-execution — reported by
/// the subcommand that was asked, so the usage line names it.
fn require_single_script(subcommand: &str, shell: Option<&str>, command: &[String]) {
    use clap::CommandFactory;

    if shell.is_none() || command.len() == 1 {
        return;
    }

    let mut cli = Cli::command();
    let mut surface = cli
        .find_subcommand_mut(subcommand)
        .expect("every caller names its own subcommand")
        .clone()
        // Taken on its own, the subcommand's usage line reads `test …` — the
        // name of the POSIX utility `rtk test` shadows. Spell the binary the
        // caller actually typed.
        .bin_name(format!("rtk {subcommand}"));
    surface
        .error(
            ErrorKind::WrongNumberOfValues,
            core::shell::SHELL_ARITY_MESSAGE,
        )
        .exit();
}

fn run_cli() -> Result<i32> {
    // Fire-and-forget telemetry ping (1/day, non-blocking)
    core::telemetry::maybe_ping();

    let cli = match Cli::try_parse_from(std::env::args_os()) {
        Ok(cli) => cli,
        Err(e) => {
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                e.exit();
            }
            return run_fallback(e);
        }
    };

    // Warn if installed hook is outdated/missing (1/day, non-blocking).
    // Skip for Gain (shows its own inline warning), Init/Verify (manage the hook themselves).
    if !matches!(
        cli.command,
        Commands::Gain { .. } | Commands::Init { .. } | Commands::Verify { .. }
    ) {
        hooks::hook_check::maybe_warn();
    }

    // Runtime integrity check for operational commands.
    // Meta commands (init, gain, verify, config, etc.) skip the check
    // because they don't go through the hook pipeline.
    if is_operational_command(&cli.command) {
        hooks::integrity::runtime_check()?;
    }

    let code = match cli.command {
        Commands::Ls { args } => ls::run(&args, cli.verbose)?,

        Commands::Tree { args } => tree::run(&args, cli.verbose)?,

        // ISSUE #989: support multiple files (cat file1 file2 → rtk read file1 file2)
        Commands::Read {
            files,
            level,
            max_lines,
            head_lines,
            tail_lines,
            line_numbers,
        } => {
            let mut had_error = false;
            let mut stdin_seen = false;
            for file in &files {
                let result = if file == Path::new("-") {
                    if stdin_seen {
                        eprintln!("rtk: warning: stdin specified more than once");
                        continue;
                    }
                    stdin_seen = true;
                    read::run_stdin(
                        level,
                        max_lines,
                        head_lines,
                        tail_lines,
                        line_numbers,
                        cli.verbose,
                    )
                } else {
                    read::run(
                        file,
                        level,
                        max_lines,
                        head_lines,
                        tail_lines,
                        line_numbers,
                        cli.verbose,
                    )
                };
                if let Err(e) = result {
                    eprintln!("cat: {}: {}", file.display(), e.root_cause());
                    had_error = true;
                }
            }
            if had_error { 1 } else { 0 }
        }

        Commands::Smart {
            file,
            model,
            force_download,
        } => {
            local_llm::run(&file, &model, force_download, cli.verbose)?;
            0
        }

        Commands::Git {
            directory,
            config_override,
            git_dir,
            work_tree,
            no_pager,
            no_optional_locks,
            bare,
            literal_pathspecs,
            command,
        } => {
            // Build global git args (inserted between "git" and subcommand)
            let mut global_args: Vec<String> = Vec::new();
            for dir in &directory {
                global_args.push("-C".to_string());
                global_args.push(dir.clone());
            }
            for cfg in &config_override {
                global_args.push("-c".to_string());
                global_args.push(cfg.clone());
            }
            if let Some(ref dir) = git_dir {
                global_args.push("--git-dir".to_string());
                global_args.push(dir.clone());
            }
            if let Some(ref tree) = work_tree {
                global_args.push("--work-tree".to_string());
                global_args.push(tree.clone());
            }
            if no_pager {
                global_args.push("--no-pager".to_string());
            }
            if no_optional_locks {
                global_args.push("--no-optional-locks".to_string());
            }
            if bare {
                global_args.push("--bare".to_string());
            }
            if literal_pathspecs {
                global_args.push("--literal-pathspecs".to_string());
            }

            match command {
                GitCommands::Diff { args } => git_cmd::run(
                    git_cmd::GitCommand::Diff,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Log { args } => git_cmd::run(
                    git_cmd::GitCommand::Log,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Status { args } => git_cmd::run(
                    git_cmd::GitCommand::Status,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Show { args } => git_cmd::run(
                    git_cmd::GitCommand::Show,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Add { args } => git_cmd::run(
                    git_cmd::GitCommand::Add,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Commit { args } => git_cmd::run(
                    git_cmd::GitCommand::Commit,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Checkout { args } => git_cmd::run(
                    git_cmd::GitCommand::Checkout,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Push { args } => git_cmd::run(
                    git_cmd::GitCommand::Push,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Pull { args } => git_cmd::run(
                    git_cmd::GitCommand::Pull,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Branch { args } => git_cmd::run(
                    git_cmd::GitCommand::Branch,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Fetch { args } => git_cmd::run(
                    git_cmd::GitCommand::Fetch,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Stash { subcommand, args } => git_cmd::run(
                    git_cmd::GitCommand::Stash { subcommand },
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Worktree { args } => git_cmd::run(
                    git_cmd::GitCommand::Worktree,
                    &args,
                    None,
                    cli.verbose,
                    &global_args,
                )?,
                GitCommands::Other(args) => {
                    git_cmd::run_passthrough(&args, &global_args, cli.verbose)?
                }
            }
        }

        Commands::Gh { subcommand, args } => {
            gh_cmd::run(&subcommand, &args, cli.verbose, cli.ultra_compact)?
        }

        Commands::Glab {
            repo,
            group,
            subcommand,
            mut args,
        } => {
            // Append -R / -g flags at end so they don't interfere with
            // subcommand dispatch (args[0] must be the sub-subcommand like "list")
            if let Some(r) = repo {
                args.push("-R".to_string());
                args.push(r);
            }
            if let Some(g) = group {
                args.push("-g".to_string());
                args.push(g);
            }
            glab_cmd::run(&subcommand, &args, cli.verbose, cli.ultra_compact)?
        }

        Commands::Aws { subcommand, args } => aws_cmd::run(&subcommand, &args, cli.verbose)?,

        Commands::Psql { args } => psql_cmd::run(&args, cli.verbose)?,

        Commands::Pnpm {
            filter,
            recursive,
            workspace_root,
            command,
        } => {
            // Warns user if filters are used with unsupported subcommands like typecheck
            if let Some(warning) = validate_pnpm_filters(&filter, &command) {
                eprintln!("{}", warning);
            }
            if let Some(warning) = validate_pnpm_globals(recursive, workspace_root, &command) {
                eprintln!("{}", warning);
            }

            match command {
                PnpmCommands::List { depth, args } => pnpm_cmd::run(
                    pnpm_cmd::PnpmCommand::List { depth },
                    &merge_pnpm_args(&filter, recursive, workspace_root, &args),
                    cli.verbose,
                )?,
                PnpmCommands::Outdated { args } => pnpm_cmd::run(
                    pnpm_cmd::PnpmCommand::Outdated,
                    &merge_pnpm_args(&filter, recursive, workspace_root, &args),
                    cli.verbose,
                )?,
                PnpmCommands::Install { args } => pnpm_cmd::run(
                    pnpm_cmd::PnpmCommand::Install,
                    &merge_pnpm_args(&filter, recursive, workspace_root, &args),
                    cli.verbose,
                )?,
                PnpmCommands::Typecheck { args } => tsc_cmd::run(Some("pnpm"), &args, cli.verbose)?,
                PnpmCommands::Other(args) => pnpm_cmd::run_passthrough(
                    &merge_pnpm_args_os(&filter, recursive, workspace_root, &args),
                    cli.verbose,
                )?,
            }
        }

        Commands::Err { shell, command } => {
            require_single_script("err", shell.as_deref(), &command);
            runner::run_err(&command, shell.as_deref(), cli.verbose)
                .context("Failed to run err command")?
        }

        Commands::Test { shell, command } => {
            require_single_script("test", shell.as_deref(), &command);
            // A native `test` expression (`rtk test -f Cargo.toml`) still goes
            // to the real `test` binary; `--shell` means the caller asked for a
            // script, so it never takes that path.
            if shell.is_none() && is_native_test_expression(&command) {
                let args: Vec<OsString> = command.into_iter().map(OsString::from).collect();
                core::runner::run_passthrough("test", &args, cli.verbose)?
            } else {
                // `rtk test ! <cmd>` negates the command's exit status. The
                // joined `sh -c` string used to get that from the shell; direct
                // execution applies it here instead, so the boundaries survive
                // and no shell is interposed for it.
                let (negations, command) = split_leading_negations(command);
                let code = runner::run_test(&command, shell.as_deref(), cli.verbose)
                    .context("Failed to run test command")?;
                if negations % 2 == 1 {
                    i32::from(code == 0)
                } else {
                    code
                }
            }
        }

        Commands::Json {
            file,
            depth,
            keys_only,
        } => {
            if file == Path::new("-") {
                json_cmd::run_stdin(depth, keys_only, cli.verbose)?;
            } else {
                json_cmd::run(&file, depth, keys_only, cli.verbose)?;
            }
            0
        }

        Commands::Deps { path } => {
            deps::run(&path, cli.verbose)?;
            0
        }

        Commands::Env { filter } => {
            env_cmd::run(filter.as_deref(), cli.verbose)?;
            0
        }

        Commands::Find { args } => find_cmd::run_from_args(&args, cli.verbose)?,

        Commands::Diff { file1, file2 } => {
            if let Some(f2) = file2 {
                diff_cmd::run(&file1, &f2, cli.verbose)?
            } else if file1.as_os_str() == "-" {
                diff_cmd::run_stdin(cli.verbose)?;
                0
            } else {
                // `diff` rejects a lone file operand as a usage error, exit 2,
                // before opening it, so `diff <file> && next` stops here too.
                eprintln!("diff: missing operand after '{}'", file1.display());
                2
            }
        }

        Commands::Log { file } => {
            if let Some(f) = file {
                log_cmd::run_file(&f, cli.verbose)?;
            } else {
                log_cmd::run_stdin(cli.verbose)?;
            }
            0
        }

        Commands::Dotnet { command } => match command {
            DotnetCommands::Build { args } => dotnet_cmd::run_build(&args, cli.verbose)?,
            DotnetCommands::Test { args } => dotnet_cmd::run_test(&args, cli.verbose)?,
            DotnetCommands::Restore { args } => dotnet_cmd::run_restore(&args, cli.verbose)?,
            DotnetCommands::Format { args } => dotnet_cmd::run_format(&args, cli.verbose)?,
            DotnetCommands::Other(args) => dotnet_cmd::run_passthrough(&args, cli.verbose)?,
        },

        Commands::Docker { command } => match command {
            DockerCommands::Ps { all } => {
                let cmd = if all {
                    container::ContainerCmd::DockerPsAll
                } else {
                    container::ContainerCmd::DockerPs
                };
                container::run(cmd, &[], cli.verbose)?
            }
            DockerCommands::Images => {
                container::run(container::ContainerCmd::DockerImages, &[], cli.verbose)?
            }
            DockerCommands::Logs { container: c } => {
                container::run(container::ContainerCmd::DockerLogs, &[c], cli.verbose)?
            }
            DockerCommands::Compose { command: compose } => match compose {
                ComposeCommands::Ps { all } => container::run_compose_ps(all, cli.verbose)?,
                ComposeCommands::Logs { service, tail } => {
                    container::run_compose_logs(service.as_deref(), tail, cli.verbose)?
                }
                ComposeCommands::Build { service } => {
                    container::run_compose_build(service.as_deref(), cli.verbose)?
                }
                ComposeCommands::Other(args) => {
                    container::run_compose_passthrough(&args, cli.verbose)?
                }
            },
            DockerCommands::Other(args) => container::run_docker_passthrough(&args, cli.verbose)?,
        },

        Commands::Kubectl { command } => match command {
            KubectlCommands::Get { args } => container::run_kubectl_get(&args, cli.verbose)?,
            KubectlCommands::Pods { namespace, all } => {
                let args = build_k8s_namespace_args(namespace, all);
                container::run(container::ContainerCmd::KubectlPods, &args, cli.verbose)?
            }
            KubectlCommands::Services { namespace, all } => {
                let args = build_k8s_namespace_args(namespace, all);
                container::run(container::ContainerCmd::KubectlServices, &args, cli.verbose)?
            }
            KubectlCommands::Logs { pod, container: c } => {
                let args = build_k8s_logs_args(pod, c);
                container::run(container::ContainerCmd::KubectlLogs, &args, cli.verbose)?
            }
            KubectlCommands::Other(args) => container::run_kubectl_passthrough(&args, cli.verbose)?,
        },

        Commands::Oc { command } => match command {
            OcCommands::Get { args } => container::run_oc_get(&args, cli.verbose)?,
            OcCommands::Pods { namespace, all } => {
                let args = build_k8s_namespace_args(namespace, all);
                container::k8s_pods("oc", &args, cli.verbose)?
            }
            OcCommands::Services { namespace, all } => {
                let args = build_k8s_namespace_args(namespace, all);
                container::k8s_services("oc", &args, cli.verbose)?
            }
            OcCommands::Logs { pod, container: c } => {
                let args = build_k8s_logs_args(pod, c);
                container::k8s_logs("oc", &args, cli.verbose)?
            }
            OcCommands::Other(args) => container::run_oc_passthrough(&args, cli.verbose)?,
        },

        Commands::Summary { shell, command } => {
            require_single_script("summary", shell.as_deref(), &command);
            summary::run(&command, shell.as_deref(), cli.verbose)
                .context("Failed to run summary command")?
        }

        Commands::Grep {
            max_len,
            max,
            context_only,
            extra_args,
        } => search::run(
            search::Engine::Grep,
            max_len,
            max,
            context_only,
            &extra_args,
            cli.verbose,
        )?,
        Commands::AstGrep { extra_args } => ast_grep_cmd::run(&extra_args)?,
        Commands::Rg { extra_args } => {
            search::run(search::Engine::Rg, 80, 200, false, &extra_args, cli.verbose)?
        }

        Commands::Init {
            global,
            opencode,
            gemini,
            agent,
            show,
            claude_md,
            hook_only,
            auto_patch,
            no_patch,
            trust_filters,
            no_trust_filters,
            uninstall,
            codex,
            copilot,
            dry_run,
        } => {
            let ctx = hooks::init::InitContext {
                verbose: cli.verbose,
                dry_run,
                awareness: if show || uninstall {
                    core::config::AwarenessLevel::default()
                } else {
                    configured_awareness_level()
                },
            };
            let patch_mode = if auto_patch {
                hooks::init::PatchMode::Auto
            } else if no_patch {
                hooks::init::PatchMode::Skip
            } else {
                hooks::init::PatchMode::Ask
            };
            if show {
                hooks::init::show_config(codex, agent == Some(AgentTarget::Omp))?;
            } else if uninstall && copilot {
                if global {
                    hooks::init::uninstall_copilot_global(ctx)?;
                } else {
                    hooks::init::uninstall_copilot(ctx)?;
                }
            } else if uninstall && agent == Some(AgentTarget::Kilocode) {
                if !global {
                    anyhow::bail!(
                        "Kilo Code plugin is user-scoped. Use: rtk init -g --agent kilocode --uninstall"
                    );
                }
                hooks::init::uninstall_kilocode(ctx)?;
            } else if uninstall {
                uninstall_init_dispatch(
                    agent,
                    global,
                    gemini,
                    codex,
                    patch_mode,
                    ctx,
                    hooks::init::uninstall_hermes,
                    hooks::init::uninstall_with_patch_mode,
                )?;
            } else if gemini {
                hooks::init::run_gemini(global, hook_only, patch_mode, ctx)?;
            } else if copilot {
                if global {
                    hooks::init::run_copilot_global(ctx)?;
                } else {
                    hooks::init::run_copilot(ctx)?;
                }
            } else if agent == Some(AgentTarget::Pi) {
                hooks::init::run_pi_mode_with_patch_mode(global, patch_mode, ctx)?
            } else if agent == Some(AgentTarget::Trae) {
                hooks::init::run_trae_mode(global, ctx)?
            } else if agent == Some(AgentTarget::Omp) {
                hooks::init::run_omp_mode_with_patch_mode(global, patch_mode, ctx)?
            } else if agent == Some(AgentTarget::Kilocode) {
                if !global {
                    anyhow::bail!(
                        "Kilo Code plugin is user-scoped. Use: rtk init -g --agent kilocode"
                    );
                }
                hooks::init::run_kilocode_mode(ctx)?;
            } else if agent == Some(AgentTarget::Antigravity) {
                hooks::init::run_antigravity_mode(global, ctx)?;
            } else if agent == Some(AgentTarget::Kimi) {
                if global {
                    anyhow::bail!("Kimi AI is project-scoped. Use: rtk init --agent kimi");
                }
                hooks::init::run_kimi_mode(ctx)?;
            } else if agent == Some(AgentTarget::Hermes) {
                hooks::init::run_hermes_mode(ctx)?;
            } else if agent == Some(AgentTarget::Droid) {
                hooks::init::run_droid_mode(global, ctx)?;
            } else if agent == Some(AgentTarget::Vibe) {
                hooks::init::run_vibe_mode(global, hook_only, patch_mode, ctx)?;
            } else {
                let install_opencode = opencode;
                let install_cursor = agent == Some(AgentTarget::Cursor);
                let install_windsurf = agent == Some(AgentTarget::Windsurf);
                let install_cline = agent == Some(AgentTarget::Cline);
                // Sibling agents (Cursor, Windsurf, Cline) fall through to this
                // shared init path; only the default (no --agent) or an explicit
                // Claude target should install Claude Code files. Without this
                // guard, `rtk init -g --agent cursor` writes into ~/.claude (#2097).
                let install_claude = !install_cursor && !install_windsurf && !install_cline;

                hooks::init::run(
                    global,
                    install_claude,
                    install_opencode,
                    install_cursor,
                    install_windsurf,
                    install_cline,
                    claude_md,
                    hook_only,
                    codex,
                    patch_mode,
                    ctx,
                )?;
                let filter_trust = if trust_filters {
                    hooks::init::FilterTrust::Trust
                } else if no_trust_filters || auto_patch {
                    hooks::init::FilterTrust::Skip
                } else {
                    hooks::init::FilterTrust::Ask
                };
                hooks::init::finalize_filter_trust(global, dry_run, filter_trust)?;
            }
            0
        }

        Commands::Wget { url, output, args } => {
            if output.as_deref() == Some("-") {
                wget_cmd::run_stdout(&url, &args, cli.verbose)?
            } else {
                // Pass -O <file> through to wget via args
                let mut all_args = Vec::new();
                if let Some(out_file) = &output {
                    all_args.push("-O".to_string());
                    all_args.push(out_file.clone());
                }
                all_args.extend(args);
                wget_cmd::run(&url, &all_args, cli.verbose)?
            }
        }

        Commands::Wc { args } => wc_cmd::run(&args, cli.verbose)?,

        Commands::Gain {
            project, // added
            graph,
            history,
            quota,
            tier,
            daily,
            weekly,
            monthly,
            all,
            format,
            failures,
            recalls,
            reset,
            yes,
        } => {
            analytics::gain::run(
                project, // added: pass project flag
                graph,
                history,
                quota,
                &tier,
                daily,
                weekly,
                monthly,
                all,
                &format,
                failures,
                recalls,
                reset,
                yes,
                cli.verbose,
            )?;
            0
        }

        Commands::CcEconomics {
            daily,
            weekly,
            monthly,
            all,
            format,
        } => {
            analytics::cc_economics::run(daily, weekly, monthly, all, &format, cli.verbose)?;
            0
        }

        Commands::Config { create, action } => {
            match action {
                Some(ConfigAction::Recall { mode: None }) => {
                    core::config::show_recall_mode()?;
                }
                Some(ConfigAction::Recall { mode: Some(mode) }) => {
                    use core::retriever::RecoveryMode;
                    let parsed = match mode.as_str() {
                        "sqlite" => RecoveryMode::Sqlite,
                        "tee" => RecoveryMode::Tee,
                        "disabled" => RecoveryMode::Disabled,
                        other => anyhow::bail!(
                            "unknown recall mode '{other}' (expected: sqlite, tee, disabled)"
                        ),
                    };
                    let path = core::config::set_recall_mode(parsed)?;
                    println!("recall mode set to {mode} in {}", path.display());
                }
                None => {
                    if create {
                        let path = core::config::Config::create_default()?;
                        println!("Created: {}", path.display());
                    } else {
                        core::config::show_config()?;
                    }
                }
            }
            0
        }

        Commands::Vitest { ref args } => vitest_cmd::run_vitest(args, cli.verbose)?,
        Commands::Jest { ref args } => vitest_cmd::run_jest(args, cli.verbose)?,

        Commands::Ctest { args } => ctest_cmd::run(&args, cli.verbose)?,

        Commands::Prisma { command } => match command {
            PrismaCommands::Generate { args } => {
                prisma_cmd::run(prisma_cmd::PrismaCommand::Generate, &args, cli.verbose)?
            }
            PrismaCommands::Migrate { command } => match command {
                PrismaMigrateCommands::Dev { name, args } => prisma_cmd::run(
                    prisma_cmd::PrismaCommand::Migrate {
                        subcommand: prisma_cmd::MigrateSubcommand::Dev { name },
                    },
                    &args,
                    cli.verbose,
                )?,
                PrismaMigrateCommands::Status { args } => prisma_cmd::run(
                    prisma_cmd::PrismaCommand::Migrate {
                        subcommand: prisma_cmd::MigrateSubcommand::Status,
                    },
                    &args,
                    cli.verbose,
                )?,
                PrismaMigrateCommands::Deploy { args } => prisma_cmd::run(
                    prisma_cmd::PrismaCommand::Migrate {
                        subcommand: prisma_cmd::MigrateSubcommand::Deploy,
                    },
                    &args,
                    cli.verbose,
                )?,
            },
            PrismaCommands::DbPush { args } => {
                prisma_cmd::run(prisma_cmd::PrismaCommand::DbPush, &args, cli.verbose)?
            }
        },

        Commands::Tsc { args } => tsc_cmd::run(None, &args, cli.verbose)?,

        Commands::Next { args } => next_cmd::run(&args, cli.verbose)?,

        Commands::Lint { args } => lint_cmd::run(None, &args, cli.verbose)?,

        Commands::Prettier { args } => prettier_cmd::run(&args, cli.verbose)?,

        Commands::Format { args } => format_cmd::run(&args, cli.verbose)?,

        Commands::Playwright { args } => playwright_cmd::run(&args, cli.verbose)?,

        Commands::Cargo { command } => match command {
            CargoCommands::Build { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Build, &args, cli.verbose)?
            }
            CargoCommands::Test { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Test, &args, cli.verbose)?
            }
            CargoCommands::Clippy { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Clippy, &args, cli.verbose)?
            }
            CargoCommands::Check { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Check, &args, cli.verbose)?
            }
            CargoCommands::Install { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Install, &args, cli.verbose)?
            }
            CargoCommands::Nextest { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Nextest, &args, cli.verbose)?
            }
            CargoCommands::Other(args) => cargo_cmd::run_passthrough(&args, cli.verbose)?,
        },

        Commands::Bun { command } => match command {
            BunCommands::Install { args } => bun_cmd::run_pkg("install", &args, cli.verbose)?,
            BunCommands::Add { args } => bun_cmd::run_pkg("add", &args, cli.verbose)?,
            BunCommands::Remove { args } => bun_cmd::run_pkg("remove", &args, cli.verbose)?,
            BunCommands::Run { args } => {
                let os_args: Vec<OsString> = std::iter::once(OsString::from("run"))
                    .chain(args.into_iter().map(OsString::from))
                    .collect();
                bun_cmd::run_passthrough(&os_args, cli.verbose)?
            }
            BunCommands::Build { args } => bun_cmd::run_build(&args, cli.verbose)?,
            BunCommands::Test { args } => bun_cmd::run_test(&args, cli.verbose)?,
            BunCommands::Pm { command } => match command {
                BunPmCommands::Ls { args } => bun_cmd::run_pm_ls(&args, cli.verbose)?,
                BunPmCommands::Other(args) => {
                    let os_args: Vec<OsString> =
                        std::iter::once(OsString::from("pm")).chain(args).collect();
                    bun_cmd::run_passthrough(&os_args, cli.verbose)?
                }
            },
            BunCommands::X { args } => run_bunx_tool(&args, cli.verbose, cli.skip_env)?,
            BunCommands::Other(args) => bun_cmd::run_passthrough(&args, cli.verbose)?,
        },

        Commands::Bunx { args } => run_bunx_tool(&args, cli.verbose, cli.skip_env)?,

        Commands::Deno { command } => match command {
            DenoCommands::Test { args } => deno_cmd::run_test(&args, cli.verbose)?,
            DenoCommands::Check { args } => deno_cmd::run_check(&args, cli.verbose)?,
            DenoCommands::Lint { args } => deno_cmd::run_lint(&args, cli.verbose)?,
            DenoCommands::Run { args } => {
                let os_args: Vec<OsString> = std::iter::once(OsString::from("run"))
                    .chain(args.into_iter().map(OsString::from))
                    .collect();
                deno_cmd::run_passthrough(&os_args, cli.verbose)?
            }
            DenoCommands::Task { args } => {
                let os_args: Vec<OsString> = std::iter::once(OsString::from("task"))
                    .chain(args.into_iter().map(OsString::from))
                    .collect();
                deno_cmd::run_passthrough(&os_args, cli.verbose)?
            }
            DenoCommands::Compile { args } => deno_cmd::run_compile(&args, cli.verbose)?,
            DenoCommands::Install { args } => {
                let os_args: Vec<OsString> = std::iter::once(OsString::from("install"))
                    .chain(args.into_iter().map(OsString::from))
                    .collect();
                deno_cmd::run_passthrough(&os_args, cli.verbose)?
            }
            DenoCommands::Other(args) => deno_cmd::run_passthrough(&args, cli.verbose)?,
        },

        Commands::Npm { args } => npm_cmd::run(&args, cli.verbose, cli.skip_env)?,

        Commands::Curl { args } => curl_cmd::run(&args, cli.verbose)?,

        Commands::Discover {
            project,
            limit,
            all,
            since,
            format,
        } => {
            discover::run(project.as_deref(), all, since, limit, &format, cli.verbose)?;
            0
        }

        Commands::Session {} => {
            analytics::session_cmd::run(cli.verbose)?;
            0
        }

        Commands::Telemetry { command } => {
            core::telemetry_cmd::run(&command)?;
            0
        }

        Commands::Learn {
            project,
            all,
            since,
            format,
            write_rules,
            min_confidence,
            min_occurrences,
        } => {
            learn::run(
                project,
                all,
                since,
                format,
                write_rules,
                min_confidence,
                min_occurrences,
            )?;
            0
        }

        Commands::Npx { args } => {
            if args.is_empty() {
                anyhow::bail!("npx requires a command argument");
            }

            // Intelligent routing: delegate to specialized filters
            match args[0].as_str() {
                "tsc" | "typescript" => tsc_cmd::run(Some("npx"), &args[1..], cli.verbose)?,
                "eslint" => lint_cmd::run(Some("npx"), &args, cli.verbose)?,
                "prisma" => {
                    // Route to prisma_cmd based on subcommand
                    if args.len() > 1 {
                        let prisma_args: Vec<String> = args[2..].to_vec();
                        match args[1].as_str() {
                            "generate" => prisma_cmd::run(
                                prisma_cmd::PrismaCommand::Generate,
                                &prisma_args,
                                cli.verbose,
                            )?,
                            "db" if args.len() > 2 && args[2] == "push" => prisma_cmd::run(
                                prisma_cmd::PrismaCommand::DbPush,
                                &args[3..],
                                cli.verbose,
                            )?,
                            _ => {
                                // Passthrough other prisma subcommands
                                let timer = core::tracking::TimedExecution::start();
                                let mut cmd = core::utils::resolved_command("npx");
                                for arg in &args {
                                    cmd.arg(arg);
                                }
                                let status = cmd.status().context("Failed to run npx prisma")?;
                                let args_str = args.join(" ");
                                timer.track_passthrough(
                                    &format!("npx {}", args_str),
                                    &format!("rtk npx {} (passthrough)", args_str),
                                );
                                core::utils::exit_code_from_status(&status, "npx prisma")
                            }
                        }
                    } else {
                        let timer = core::tracking::TimedExecution::start();
                        let status = core::utils::resolved_command("npx")
                            .arg("prisma")
                            .status()
                            .context("Failed to run npx prisma")?;
                        timer.track_passthrough("npx prisma", "rtk npx prisma (passthrough)");
                        core::utils::exit_code_from_status(&status, "npx prisma")
                    }
                }
                "next" => next_cmd::run(&args[1..], cli.verbose)?,
                "prettier" => prettier_cmd::run(&args[1..], cli.verbose)?,
                "playwright" => playwright_cmd::run(&args[1..], cli.verbose)?,
                _ => npm_cmd::exec(&args, cli.verbose, cli.skip_env)?,
            }
        }

        Commands::Ruff { args } => ruff_cmd::run(&args, cli.verbose)?,

        Commands::Sqlfluff { args } => sqlfluff_cmd::run(&args, cli.verbose)?,

        Commands::Pytest { args } => pytest_cmd::run(&args, cli.verbose)?,

        Commands::Mypy { args } => mypy_cmd::run(&args, cli.verbose)?,

        Commands::Php { args } => php_cmd::run(&args, cli.verbose)?,

        Commands::Phpunit { args } => phpunit_cmd::run(&args, cli.verbose)?,

        Commands::Phpstan { args } => phpstan_cmd::run(&args, cli.verbose)?,

        Commands::Pest { args } => pest_cmd::run(&args, cli.verbose)?,

        Commands::Paratest { args } => paratest_cmd::run(&args, cli.verbose)?,

        Commands::Ecs { args } => ecs_cmd::run(&args, cli.verbose)?,

        Commands::Pint { args } => pint_cmd::run(&args, cli.verbose)?,

        Commands::Phpt { args } => phpt_cmd::run(&args, cli.verbose)?,

        Commands::Rake { args } => rake_cmd::run(&args, cli.verbose)?,

        Commands::Rubocop { args } => rubocop_cmd::run(&args, cli.verbose)?,

        Commands::Rspec { args } => rspec_cmd::run(&args, cli.verbose)?,

        Commands::Pip { args } => pip_cmd::run(&args, cli.verbose)?,

        Commands::Uv { args } => uv_cmd::run(&args, cli.verbose)?,

        Commands::Go { command } => match command {
            GoCommands::Test { args } => go_cmd::run_test(&args, cli.verbose)?,
            GoCommands::Build { args } => go_cmd::run_build(&args, cli.verbose)?,
            GoCommands::Vet { args } => go_cmd::run_vet(&args, cli.verbose)?,
            GoCommands::Other(args) => go_cmd::run_other(&args, cli.verbose)?,
        },

        Commands::Sbt { command } => match command {
            SbtCommands::Test { args } => sbt_cmd::run_test(&args, cli.verbose)?,
            SbtCommands::Compile { args } => sbt_cmd::run_compile(&args, cli.verbose)?,
            SbtCommands::Run { args } => sbt_cmd::run_run(&args, cli.verbose)?,
            SbtCommands::Other(args) => sbt_cmd::run_other(&args, cli.verbose)?,
        },

        Commands::Gt { command } => match command {
            GtCommands::Log { args } => gt_cmd::run_log(&args, cli.verbose)?,
            GtCommands::Submit { args } => gt_cmd::run_submit(&args, cli.verbose)?,
            GtCommands::Sync { args } => gt_cmd::run_sync(&args, cli.verbose)?,
            GtCommands::Restack { args } => gt_cmd::run_restack(&args, cli.verbose)?,
            GtCommands::Create { args } => gt_cmd::run_create(&args, cli.verbose)?,
            GtCommands::Branch { args } => gt_cmd::run_branch(&args, cli.verbose)?,
            GtCommands::Other(args) => gt_cmd::run_other(&args, cli.verbose)?,
        },

        Commands::GolangciLint { args } => golangci_cmd::run(&args, cli.verbose)?,

        Commands::Gradlew { args } => gradlew_cmd::run(&args, cli.verbose)?,

        Commands::Mvn { args } => mvn_cmd::run(&args, cli.verbose)?,

        Commands::Mvnd { args } => mvn_cmd::run_daemon(&args, cli.verbose)?,

        Commands::HookAudit { since } => {
            hooks::hook_audit_cmd::run(since, cli.verbose)?;
            0
        }

        Commands::Hook { command } => match command {
            HookCommands::Claude => {
                hooks::hook_cmd::run_claude()?;
                0
            }
            HookCommands::Trae => {
                hooks::hook_cmd::run_trae()?;
                0
            }
            HookCommands::Codex => {
                hooks::hook_cmd::run_codex()?;
                0
            }
            HookCommands::Cursor => {
                hooks::hook_cmd::run_cursor()?;
                0
            }
            HookCommands::Gemini => {
                hooks::hook_cmd::run_gemini()?;
                0
            }
            HookCommands::Copilot => {
                hooks::hook_cmd::run_copilot()?;
                0
            }
            HookCommands::Droid => {
                hooks::hook_cmd::run_droid()?;
                0
            }
            HookCommands::Antigravity => {
                hooks::hook_cmd::run_antigravity()?;
                0
            }
            HookCommands::Vibe => {
                hooks::hook_cmd::run_vibe()?;
                0
            }
            HookCommands::Check { agent, command } => {
                // Answers the same question the hooks answer, through the same
                // decision (`hooks::decision`) — not just "does a rewrite rule
                // match?". Checking the rule alone reported a rewrite for
                // command substitutions, file redirects and heredocs that both
                // hook paths refuse to touch, which is the opposite of what a
                // diagnostic is for.
                use crate::hooks::decision::{AgentPath, HookDecision};
                let raw = command.join(" ");
                // Answer for the agent that was asked about. Agents differ both
                // in whose permission rules their hook reads and in how it
                // decides -- see `AgentPath` -- so one hard-coded answer would
                // misdescribe the very hook being diagnosed.
                let Some(path) = AgentPath::from_agent(&agent) else {
                    return Ok(2);
                };
                match path.decide(&raw) {
                    HookDecision::AllowRewrite(rewritten) | HookDecision::AskRewrite(rewritten) => {
                        println!("{}", rewritten);
                        0
                    }
                    HookDecision::Deny => {
                        eprintln!("Denied by a permission rule: {}", raw);
                        1
                    }
                    HookDecision::Defer => {
                        eprintln!("No rewrite for: {}", raw);
                        1
                    }
                }
            }
        },

        Commands::Rewrite { args } => {
            let cmd = args.join(" ");
            hooks::rewrite_cmd::run(&cmd)?;
            0
        }

        Commands::Pipe {
            filter,
            passthrough,
        } => {
            pipe_cmd::run(filter.as_deref(), passthrough)?;
            0
        }

        Commands::Run {
            command,
            shell,
            args,
        } => {
            if command
                .as_deref()
                .is_none_or(|script| script.trim().is_empty())
                && args.is_empty()
            {
                0
            } else {
                let (launch, description, program) = match command {
                    Some(script) => (
                        core::shell::shell_command(&script, shell.as_deref())
                            .context("Failed to prepare run shell command")?,
                        format!("shell command: {script}"),
                        core::shell::program_name(&[], shell.as_deref()).to_string(),
                    ),
                    None => (
                        core::shell::direct_command(&args)
                            .context("Failed to prepare direct run command")?,
                        core::shell::display_args(&args),
                        core::shell::program_name(&args, None).to_string(),
                    ),
                };
                match launch {
                    core::shell::Launch::Ready(mut prepared) => match prepared.status() {
                        Ok(status) => core::utils::exit_code_from_status(&status, "run"),
                        Err(error) => {
                            let error = anyhow::Error::new(error)
                                .context(format!("Failed to execute {description}"));
                            match core::shell::spawn_failure(&program, &error) {
                                Some(outcome) => {
                                    eprint!("{}", outcome.message);
                                    outcome.code
                                }
                                None => return Err(error),
                            }
                        }
                    },
                    // `rtk run` is a raw passthrough, so it reports a program it
                    // cannot run the way a shell does: the message on stderr and
                    // the shell's own exit code.
                    core::shell::Launch::Unrunnable(outcome) => {
                        eprint!("{}", outcome.message);
                        outcome.code
                    }
                }
            }
        }

        Commands::Recall {
            hash,
            full,
            from,
            lines,
            grep,
            list,
        } => core::retriever::run_recall(core::retriever::RecallArgs {
            hash: hash.as_deref(),
            full,
            from,
            lines,
            grep: grep.as_deref(),
            list,
        })?,

        Commands::Proxy { args } => {
            use crate::core::utils::ChildArgExt;
            use std::io::{Read, Write};
            use std::process::Stdio;
            use std::sync::atomic::{AtomicU32, Ordering};
            use std::thread;

            if args.is_empty() {
                anyhow::bail!(
                    "proxy requires a command to execute\nUsage: rtk proxy <command> [args...]"
                );
            }

            let timer = core::tracking::TimedExecution::start();

            // If a single quoted arg contains spaces, split it respecting quotes (#388).
            // e.g. rtk proxy 'head -50 file.php' → cmd=head, args=["-50", "file.php"]
            // e.g. rtk proxy 'git log --format="%H %s"' → cmd=git, args=["log", "--format=%H %s"]
            let (cmd_name, cmd_args): (String, Vec<String>) = if args.len() == 1 {
                let full = args[0].to_string_lossy();
                let parts = shell_split(&full);
                if parts.len() > 1 {
                    (parts[0].clone(), parts[1..].to_vec())
                } else {
                    (full.into_owned(), vec![])
                }
            } else {
                (
                    args[0].to_string_lossy().into_owned(),
                    args[1..]
                        .iter()
                        .map(|s| s.to_string_lossy().into_owned())
                        .collect(),
                )
            };

            if cli.verbose > 0 {
                eprintln!("Proxy mode: {} {}", cmd_name, cmd_args.join(" "));
            }

            // ISSUE #897: Kill proxy child on SIGINT/SIGTERM to prevent orphan
            // processes. Drop-based ChildGuard doesn't run on signals with
            // panic=abort, so we register a signal handler that kills the child
            // PID stored in this atomic.
            static PROXY_CHILD_PID: AtomicU32 = AtomicU32::new(0);

            #[cfg(unix)]
            #[allow(unsafe_code)]
            {
                unsafe extern "C" fn handle_signal(sig: libc::c_int) {
                    let pid = PROXY_CHILD_PID.load(Ordering::SeqCst);
                    if pid != 0 {
                        // nosemgrep: unsafe-block
                        unsafe {
                            libc::kill(pid as libc::pid_t, libc::SIGTERM);
                            libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), 0);
                        }
                    }
                    // nosemgrep: unsafe-block
                    unsafe {
                        libc::signal(sig, libc::SIG_DFL);
                        libc::raise(sig);
                    }
                }
                // nosemgrep: unsafe-block
                unsafe {
                    libc::signal(
                        libc::SIGINT,
                        handle_signal as *const () as libc::sighandler_t,
                    );
                    libc::signal(
                        libc::SIGTERM,
                        handle_signal as *const () as libc::sighandler_t,
                    );
                }
            }

            struct ChildGuard(Option<std::process::Child>);
            impl Drop for ChildGuard {
                fn drop(&mut self) {
                    if let Some(mut child) = self.0.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    PROXY_CHILD_PID.store(0, Ordering::SeqCst);
                }
            }

            let mut child = ChildGuard(Some(
                core::utils::resolved_command(cmd_name.as_ref())
                    .child_args(&cmd_args)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .context(format!("Failed to execute command: {}", cmd_name))?,
            ));

            // Store child PID for signal handler before anything can fail
            if let Some(ref inner) = child.0 {
                PROXY_CHILD_PID.store(inner.id(), Ordering::SeqCst);
            }

            let inner = child.0.as_mut().context("Child process missing")?;
            let stdout_pipe = inner
                .stdout
                .take()
                .context("Failed to capture child stdout")?;
            let stderr_pipe = inner
                .stderr
                .take()
                .context("Failed to capture child stderr")?;

            const CAP: usize = 1_048_576;

            let stdout_handle = thread::spawn(move || -> std::io::Result<Vec<u8>> {
                let mut reader = stdout_pipe;
                let mut captured = Vec::new();
                let mut buf = [0u8; 8192];

                loop {
                    let count = reader.read(&mut buf)?;
                    if count == 0 {
                        break;
                    }
                    if captured.len() < CAP {
                        let take = count.min(CAP - captured.len());
                        captured.extend_from_slice(&buf[..take]);
                    }
                    let mut out = std::io::stdout().lock();
                    out.write_all(&buf[..count])?;
                    out.flush()?;
                }

                Ok(captured)
            });

            let stderr_handle = thread::spawn(move || -> std::io::Result<Vec<u8>> {
                let mut reader = stderr_pipe;
                let mut captured = Vec::new();
                let mut buf = [0u8; 8192];

                loop {
                    let count = reader.read(&mut buf)?;
                    if count == 0 {
                        break;
                    }
                    if captured.len() < CAP {
                        let take = count.min(CAP - captured.len());
                        captured.extend_from_slice(&buf[..take]);
                    }
                    let mut err = std::io::stderr().lock();
                    err.write_all(&buf[..count])?;
                    err.flush()?;
                }

                Ok(captured)
            });

            let status = child
                .0
                .take()
                .context("Child process missing")?
                .wait()
                .context(format!("Failed waiting for command: {}", cmd_name))?;

            let stdout_bytes = stdout_handle
                .join()
                .map_err(|_| anyhow::anyhow!("stdout streaming thread panicked"))??;
            let stderr_bytes = stderr_handle
                .join()
                .map_err(|_| anyhow::anyhow!("stderr streaming thread panicked"))??;

            let stdout = core::utils::decode_process_output(&stdout_bytes);
            let stderr = core::utils::decode_process_output(&stderr_bytes);
            let full_output = format!("{}{}", stdout, stderr);

            // Track usage (input = output since no filtering)
            timer.track(
                &format!("{} {}", cmd_name, cmd_args.join(" ")),
                &format!("rtk proxy {} {}", cmd_name, cmd_args.join(" ")),
                &full_output,
                &full_output,
            );

            core::utils::exit_code_from_status(&status, &cmd_name)
        }

        Commands::Trust { list, yes } => {
            hooks::trust::run_trust(list, yes)?;
            0
        }

        Commands::Untrust => {
            hooks::trust::run_untrust()?;
            0
        }

        Commands::Verify {
            filter,
            require_all,
        } => {
            if filter.is_some() {
                // Filter-specific mode: run only that filter's tests
                hooks::verify_cmd::run(filter, require_all)?;
            } else {
                // Default or --require-all: always run integrity check first
                hooks::integrity::run_verify(cli.verbose)?;
                hooks::verify_cmd::run(None, require_all)?;
            }
            0
        }
    };

    Ok(code)
}

/// Returns true for commands that are invoked via the hook pipeline
/// (i.e., commands that process rewritten shell commands).
/// Meta commands (init, gain, verify, etc.) are excluded because
/// they are run directly by the user, not through the hook.
/// Returns true for commands that go through the hook pipeline
/// and therefore require integrity verification.
///
/// SECURITY: whitelist pattern — new commands are NOT integrity-checked
/// until explicitly added here. A forgotten command fails open (no check)
/// rather than creating false confidence about what's protected.
fn is_operational_command(cmd: &Commands) -> bool {
    matches!(
        cmd,
        Commands::Ls { .. }
            | Commands::Tree { .. }
            | Commands::Read { .. }
            | Commands::Smart { .. }
            | Commands::Git { .. }
            | Commands::Gh { .. }
            | Commands::Glab { .. }
            | Commands::Pnpm { .. }
            | Commands::Err { .. }
            | Commands::Test { .. }
            | Commands::Json { .. }
            | Commands::Deps { .. }
            | Commands::Env { .. }
            | Commands::Find { .. }
            | Commands::Diff { .. }
            | Commands::Log { .. }
            | Commands::Dotnet { .. }
            | Commands::Docker { .. }
            | Commands::Kubectl { .. }
            | Commands::Oc { .. }
            | Commands::Summary { .. }
            | Commands::Grep { .. }
            | Commands::Rg { .. }
            | Commands::AstGrep { .. }
            | Commands::Wget { .. }
            | Commands::Jest { .. }
            | Commands::Vitest { .. }
            | Commands::Ctest { .. }
            | Commands::Prisma { .. }
            | Commands::Tsc { .. }
            | Commands::Next { .. }
            | Commands::Lint { .. }
            | Commands::Prettier { .. }
            | Commands::Playwright { .. }
            | Commands::Cargo { .. }
            | Commands::Npm { .. }
            | Commands::Npx { .. }
            | Commands::Curl { .. }
            | Commands::Ruff { .. }
            | Commands::Sqlfluff { .. }
            | Commands::Pytest { .. }
            | Commands::Php { .. }
            | Commands::Phpunit { .. }
            | Commands::Phpstan { .. }
            | Commands::Pest { .. }
            | Commands::Paratest { .. }
            | Commands::Ecs { .. }
            | Commands::Pint { .. }
            | Commands::Phpt { .. }
            | Commands::Rake { .. }
            | Commands::Rubocop { .. }
            | Commands::Rspec { .. }
            | Commands::Pip { .. }
            | Commands::Uv { .. }
            | Commands::Go { .. }
            | Commands::Sbt { .. }
            | Commands::GolangciLint { .. }
            | Commands::Gt { .. }
            | Commands::Bun { .. }
            | Commands::Bunx { .. }
            | Commands::Deno { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_isolation;
    use clap::Parser;
    use std::cell::Cell;

    #[test]
    fn test_jest_and_vitest_get_the_hook_integrity_check() {
        for framework in ["jest", "vitest"] {
            let cli = Cli::try_parse_from(["rtk", framework, "src/a.test.js"])
                .expect("rtk <framework> <path> parses");
            assert!(is_operational_command(&cli.command), "{framework}");
        }
    }

    #[test]
    fn test_git_commit_single_message() {
        let cli = Cli::try_parse_from(["rtk", "git", "commit", "-m", "fix: typo"]).unwrap();
        match cli.command {
            Commands::Git {
                command: GitCommands::Commit { args },
                ..
            } => {
                assert_eq!(args, vec!["-m", "fix: typo"]);
            }
            _ => panic!("Expected Git Commit command"),
        }
    }

    #[test]
    fn test_git_commit_multiple_messages() {
        let cli = Cli::try_parse_from([
            "rtk",
            "git",
            "commit",
            "-m",
            "feat: add support",
            "-m",
            "Body paragraph here.",
        ])
        .unwrap();
        match cli.command {
            Commands::Git {
                command: GitCommands::Commit { args },
                ..
            } => {
                assert_eq!(
                    args,
                    vec!["-m", "feat: add support", "-m", "Body paragraph here."]
                );
            }
            _ => panic!("Expected Git Commit command"),
        }
    }

    // #327: git commit -am "msg" was rejected by Clap
    #[test]
    fn test_git_commit_am_flag() {
        let cli = Cli::try_parse_from(["rtk", "git", "commit", "-am", "quick fix"]).unwrap();
        match cli.command {
            Commands::Git {
                command: GitCommands::Commit { args },
                ..
            } => {
                assert_eq!(args, vec!["-am", "quick fix"]);
            }
            _ => panic!("Expected Git Commit command"),
        }
    }

    #[test]
    fn test_git_commit_amend() {
        let cli =
            Cli::try_parse_from(["rtk", "git", "commit", "--amend", "-m", "new msg"]).unwrap();
        match cli.command {
            Commands::Git {
                command: GitCommands::Commit { args },
                ..
            } => {
                assert_eq!(args, vec!["--amend", "-m", "new msg"]);
            }
            _ => panic!("Expected Git Commit command"),
        }
    }

    #[test]
    fn test_git_global_options_parsing() {
        let cli =
            Cli::try_parse_from(["rtk", "git", "--no-pager", "--no-optional-locks", "status"])
                .unwrap();
        match cli.command {
            Commands::Git {
                no_pager,
                no_optional_locks,
                bare,
                literal_pathspecs,
                ..
            } => {
                assert!(no_pager);
                assert!(no_optional_locks);
                assert!(!bare);
                assert!(!literal_pathspecs);
            }
            _ => panic!("Expected Git command"),
        }
    }

    #[test]
    fn test_pnpm_recursive_install_parsing() {
        // The rewriter emits `rtk pnpm -r install` for `pnpm -r install`; it must parse
        // to Install with recursive=true (not error, not Other) so `-r` is forwarded.
        let cli = Cli::try_parse_from(["rtk", "pnpm", "-r", "install"]).unwrap();
        match cli.command {
            Commands::Pnpm {
                recursive,
                workspace_root,
                command,
                ..
            } => {
                assert!(recursive);
                assert!(!workspace_root);
                assert!(matches!(command, PnpmCommands::Install { .. }));
            }
            _ => panic!("Expected Pnpm command"),
        }
    }

    #[test]
    fn test_pnpm_workspace_root_filter_install_parsing() {
        let cli =
            Cli::try_parse_from(["rtk", "pnpm", "-w", "--filter", "@app", "install"]).unwrap();
        match cli.command {
            Commands::Pnpm {
                filter,
                recursive,
                workspace_root,
                command,
            } => {
                assert!(!recursive);
                assert!(workspace_root);
                assert_eq!(filter, vec!["@app".to_string()]);
                assert!(matches!(command, PnpmCommands::Install { .. }));
            }
            _ => panic!("Expected Pnpm command"),
        }
    }

    #[test]
    fn test_git_commit_long_flag_multiple() {
        let cli = Cli::try_parse_from([
            "rtk",
            "git",
            "commit",
            "--message",
            "title",
            "--message",
            "body",
            "--message",
            "footer",
        ])
        .unwrap();
        match cli.command {
            Commands::Git {
                command: GitCommands::Commit { args },
                ..
            } => {
                assert_eq!(
                    args,
                    vec![
                        "--message",
                        "title",
                        "--message",
                        "body",
                        "--message",
                        "footer"
                    ]
                );
            }
            _ => panic!("Expected Git Commit command"),
        }
    }

    #[test]
    fn test_try_parse_valid_git_status() {
        let result = Cli::try_parse_from(["rtk", "git", "status"]);
        assert!(result.is_ok(), "git status should parse successfully");
    }

    #[test]
    fn test_try_parse_init_agent_hermes() {
        let cli = Cli::try_parse_from(["rtk", "init", "--agent", "hermes"]).unwrap();
        match cli.command {
            Commands::Init { agent, .. } => {
                assert_eq!(agent, Some(AgentTarget::Hermes));
            }
            _ => panic!("Expected Init command"),
        }
    }

    #[test]
    fn test_try_parse_init_agent_trae_and_uninstall() {
        let cli = Cli::try_parse_from(["rtk", "init", "--agent", "trae", "--uninstall"]).unwrap();
        match cli.command {
            Commands::Init {
                agent, uninstall, ..
            } => {
                assert_eq!(agent, Some(AgentTarget::Trae));
                assert!(uninstall);
            }
            _ => panic!("Expected Init command"),
        }
    }

    #[test]
    fn test_try_parse_kubectl_get_alias() {
        let cli = Cli::try_parse_from(["rtk", "kubectl", "get", "pods", "-n", "default"]).unwrap();

        match cli.command {
            Commands::Kubectl {
                command: KubectlCommands::Get { args },
            } => assert_eq!(args, vec!["pods", "-n", "default"]),
            _ => panic!("Expected Kubectl Get command"),
        }
    }

    #[test]
    fn test_try_parse_oc_get() {
        let cli = Cli::try_parse_from(["rtk", "oc", "get", "pods", "-n", "default"]).unwrap();

        match cli.command {
            Commands::Oc {
                command: OcCommands::Get { args },
            } => assert_eq!(args, vec!["pods", "-n", "default"]),
            _ => panic!("Expected Oc Get command"),
        }
    }

    #[test]
    fn test_try_parse_oc_other() {
        let cli = Cli::try_parse_from(["rtk", "oc", "new-project", "test"]).unwrap();

        match cli.command {
            Commands::Oc {
                command: OcCommands::Other(_),
            } => {}
            _ => panic!("Expected Oc Other command"),
        }
    }

    #[test]
    fn test_try_parse_grep_dash_m_is_max_count() {
        // Regression: `-m` is GNU grep's --max-count, not RTK's --max. It must
        // parse (not consume the pattern), keep `max` at its default, and reach
        // extra_args so it is forwarded to grep as a real --max-count.
        let cli = Cli::try_parse_from(["rtk", "grep", "-m", "5", "pattern", "file"]).unwrap();

        match cli.command {
            Commands::Grep {
                max, extra_args, ..
            } => {
                assert_eq!(max, 200, "max must stay at its default, not consume `-m`");
                assert_eq!(extra_args, vec!["-m", "5", "pattern", "file"]);
            }
            _ => panic!("Expected Grep command"),
        }
    }

    #[test]
    fn test_try_parse_init_agent_hermes_uninstall() {
        let cli = Cli::try_parse_from(["rtk", "init", "--agent", "hermes", "--uninstall"]).unwrap();
        match cli.command {
            Commands::Init {
                agent, uninstall, ..
            } => {
                assert_eq!(agent, Some(AgentTarget::Hermes));
                assert!(uninstall);
            }
            _ => panic!("Expected Init command"),
        }
    }

    #[test]
    fn test_init_uninstall_dispatch_routes_hermes_to_hermes_cleanup() {
        let hermes_called = Cell::new(false);
        let standard_called = Cell::new(false);
        let ctx = hooks::init::InitContext {
            verbose: 2,
            dry_run: true,
            ..Default::default()
        };

        let result = uninstall_init_dispatch(
            Some(AgentTarget::Hermes),
            true,
            false,
            false,
            hooks::init::PatchMode::Ask,
            ctx,
            |ctx| {
                hermes_called.set(true);
                assert_eq!(ctx.verbose, 2);
                assert!(ctx.dry_run);
                Ok(())
            },
            |_, _, _, _, _, _, _, _| {
                standard_called.set(true);
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(hermes_called.get());
        assert!(!standard_called.get());
    }

    #[test]
    fn test_try_parse_init_agent_omp() {
        let cli = Cli::try_parse_from(["rtk", "init", "--agent", "omp"]).unwrap();
        match cli.command {
            Commands::Init { agent, .. } => {
                assert_eq!(agent, Some(AgentTarget::Omp));
            }
            _ => panic!("Expected Init command"),
        }
    }

    #[test]
    fn test_try_parse_init_agent_omp_uninstall() {
        let cli = Cli::try_parse_from(["rtk", "init", "--uninstall", "--agent", "omp", "--global"])
            .unwrap();
        match cli.command {
            Commands::Init {
                uninstall,
                agent,
                global,
                ..
            } => {
                assert!(uninstall);
                assert_eq!(agent, Some(AgentTarget::Omp));
                assert!(global);
            }
            _ => panic!("Expected Init command"),
        }
    }

    #[test]
    fn test_init_uninstall_dispatch_routes_omp_to_standard_cleanup() {
        let hermes_called = Cell::new(false);
        let standard_called = Cell::new(false);
        let ctx = hooks::init::InitContext::default();

        let result = uninstall_init_dispatch(
            Some(AgentTarget::Omp),
            true,
            false,
            false,
            hooks::init::PatchMode::Auto,
            ctx,
            |_c| {
                hermes_called.set(true);
                Ok(())
            },
            |global, _, _, _, _, omp, patch_mode, _| {
                standard_called.set(true);
                assert!(global);
                assert!(omp);
                assert_eq!(patch_mode, hooks::init::PatchMode::Auto);
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(!hermes_called.get());
        assert!(standard_called.get());
    }

    #[test]
    fn test_try_parse_help_is_display_help() {
        match Cli::try_parse_from(["rtk", "--help"]) {
            Err(e) => assert_eq!(e.kind(), ErrorKind::DisplayHelp),
            Ok(_) => panic!("Expected DisplayHelp error"),
        }
    }

    #[test]
    fn test_try_parse_version_is_display_version() {
        match Cli::try_parse_from(["rtk", "--version"]) {
            Err(e) => assert_eq!(e.kind(), ErrorKind::DisplayVersion),
            Ok(_) => panic!("Expected DisplayVersion error"),
        }
    }

    #[test]
    fn test_try_parse_unknown_subcommand_is_error() {
        match Cli::try_parse_from(["rtk", "nonexistent-command"]) {
            Err(e) => assert!(!matches!(
                e.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            )),
            Ok(_) => panic!("Expected parse error for unknown subcommand"),
        }
    }

    #[test]
    fn test_dash_d_routes_to_native_test_expression() {
        let command = vec!["-d".to_string(), "graphify-out".to_string()];
        assert!(is_native_test_expression(&command));
    }

    #[test]
    fn test_bang_before_command_is_not_a_native_expression() {
        let command = vec!["!".to_string(), "false".to_string()];
        assert!(!is_native_test_expression(&command));
    }

    #[test]
    fn test_bang_before_operator_is_a_native_expression() {
        let command = vec!["!".to_string(), "-d".to_string(), "dir".to_string()];
        assert!(is_native_test_expression(&command));
    }

    #[test]
    fn test_cargo_test_stays_test_runner() {
        let command = vec!["cargo".to_string(), "test".to_string()];
        assert!(!is_native_test_expression(&command));
    }

    #[test]
    fn test_try_parse_git_with_dash_c_succeeds() {
        let result = Cli::try_parse_from(["rtk", "git", "-C", "/path", "status"]);
        assert!(
            result.is_ok(),
            "git -C /path status should parse successfully"
        );
        if let Ok(cli) = result {
            match cli.command {
                Commands::Git { directory, .. } => {
                    assert_eq!(directory, vec!["/path"]);
                }
                _ => panic!("Expected Git command"),
            }
        }
    }

    #[test]
    fn test_gain_failures_flag_parses() {
        let result = Cli::try_parse_from(["rtk", "gain", "--failures"]);
        assert!(result.is_ok());
        if let Ok(cli) = result {
            match cli.command {
                Commands::Gain { failures, .. } => assert!(failures),
                _ => panic!("Expected Gain command"),
            }
        }
    }

    #[test]
    fn test_gain_failures_short_flag_parses() {
        let result = Cli::try_parse_from(["rtk", "gain", "-F"]);
        assert!(result.is_ok());
        if let Ok(cli) = result {
            match cli.command {
                Commands::Gain { failures, .. } => assert!(failures),
                _ => panic!("Expected Gain command"),
            }
        }
    }

    #[test]
    fn test_meta_commands_reject_bad_flags() {
        // RTK meta-commands should produce parse errors (not fall through to raw execution).
        // Skip "proxy" because it uses trailing_var_arg (accepts any args by design).
        for cmd in core::constants::RTK_META_COMMANDS {
            if matches!(
                *cmd,
                "proxy" | "run" | "rewrite" | "session" | "err" | "summary"
            ) {
                continue; // these use trailing_var_arg (accept any args by design)
            }
            let result = Cli::try_parse_from(["rtk", cmd, "--nonexistent-flag-xyz"]);
            assert!(
                result.is_err(),
                "Meta-command '{}' with bad flag should fail to parse",
                cmd
            );
        }
    }

    /// Every subcommand must be in `RTK_META_COMMANDS` (fail closed) or
    /// `PASSTHROUGH` (falls through to the real binary). Fails for any new
    /// command until it's classified.
    #[test]
    fn test_every_subcommand_is_classified() {
        use clap::CommandFactory;

        const PASSTHROUGH: &[&str] = &[
            "ls",
            "tree",
            "read",
            "rg",
            "ast-grep",
            "git",
            "gh",
            "glab",
            "aws",
            "psql",
            "pnpm",
            "test",
            "env",
            "find",
            "diff",
            "log",
            "dotnet",
            "docker",
            "kubectl",
            "oc",
            "grep",
            "wget",
            "wc",
            "jest",
            "vitest",
            "ctest",
            "prisma",
            "tsc",
            "next",
            "lint",
            "prettier",
            "format",
            "playwright",
            "cargo",
            "npm",
            "npx",
            "curl",
            "ruff",
            "sqlfluff",
            "pytest",
            "mypy",
            "rake",
            "rubocop",
            "rspec",
            "pip",
            "go",
            "gt",
            "golangci-lint",
            "gradlew",
            "mvn",
            "mvnd",
            "sbt",
            "php",
            "phpunit",
            "phpstan",
            "pest",
            "paratest",
            "ecs",
            "pint",
            "phpt",
            "uv",
            "bun",
            "bunx",
            "deno",
        ];

        let unclassified: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .filter(|name| {
                !core::constants::RTK_META_COMMANDS.contains(&name.as_str())
                    && !PASSTHROUGH.contains(&name.as_str())
            })
            .collect();

        assert!(
            unclassified.is_empty(),
            "unclassified subcommand(s) {:?}: add to RTK_META_COMMANDS (no system \
             binary) or PASSTHROUGH (wraps a real tool)",
            unclassified
        );
    }

    #[test]
    fn test_run_command_with_dash_c() {
        let cli = Cli::try_parse_from(["rtk", "run", "-c", "git status && echo done"]).unwrap();
        match cli.command {
            Commands::Run {
                command,
                shell,
                args,
            } => {
                assert_eq!(command, Some("git status && echo done".to_string()));
                assert!(shell.is_none());
                assert!(args.is_empty());
            }
            _ => panic!("Expected Run command"),
        }
    }

    #[test]
    fn test_run_command_positional_args() {
        let cli = Cli::try_parse_from(["rtk", "run", "echo", "hello"]).unwrap();
        match cli.command {
            Commands::Run {
                command,
                shell,
                args,
            } => {
                assert!(command.is_none());
                assert!(shell.is_none());
                assert_eq!(args, vec!["echo", "hello"]);
            }
            _ => panic!("Expected Run command"),
        }
    }

    #[test]
    fn test_ctest_help_and_version_passthrough_args() {
        for flag in ["--help", "--version"] {
            let cli = Cli::try_parse_from(["rtk", "ctest", flag]).unwrap();
            match cli.command {
                Commands::Ctest { args } => assert_eq!(args, vec![flag]),
                _ => panic!("Expected Ctest command"),
            }
        }
    }

    #[test]
    fn test_run_command_with_explicit_shell() {
        let cli =
            Cli::try_parse_from(["rtk", "run", "--shell", "fish", "-c", "echo (pwd)"]).unwrap();
        match cli.command {
            Commands::Run {
                command,
                shell,
                args,
            } => {
                assert_eq!(command, Some("echo (pwd)".to_string()));
                assert_eq!(shell, Some("fish".to_string()));
                assert!(args.is_empty());
            }
            _ => panic!("Expected Run command"),
        }
    }

    #[test]
    fn test_run_shell_requires_command_string() {
        assert!(Cli::try_parse_from(["rtk", "run", "--shell", "fish", "echo"]).is_err());
    }

    #[test]
    fn test_hook_claude_parses() {
        let cli = Cli::try_parse_from(["rtk", "hook", "claude"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Hook {
                command: HookCommands::Claude
            }
        ));
    }

    #[test]
    fn test_hook_trae_parses() {
        let cli = Cli::try_parse_from(["rtk", "hook", "trae"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_hook_codex_parses() {
        let cli = Cli::try_parse_from(["rtk", "hook", "codex"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Hook {
                command: HookCommands::Codex
            }
        ));
    }

    #[test]
    fn test_hook_check_parses() {
        let cli = Cli::try_parse_from(["rtk", "hook", "check", "git", "status"]).unwrap();
        match cli.command {
            Commands::Hook {
                command: HookCommands::Check { agent, command },
            } => {
                assert_eq!(agent, "claude");
                assert_eq!(command, vec!["git", "status"]);
            }
            _ => panic!("Expected Hook Check command"),
        }
    }

    #[test]
    fn test_hook_check_with_agent() {
        let cli =
            Cli::try_parse_from(["rtk", "hook", "check", "--agent", "gemini", "cargo", "test"])
                .unwrap();
        match cli.command {
            Commands::Hook {
                command: HookCommands::Check { agent, command },
            } => {
                assert_eq!(agent, "gemini");
                assert_eq!(command, vec!["cargo", "test"]);
            }
            _ => panic!("Expected Hook Check command"),
        }
    }

    #[test]
    fn test_hook_check_preserves_double_dash_in_command() {
        let cli = Cli::try_parse_from([
            "rtk",
            "hook",
            "check",
            "shadowenv",
            "exec",
            "--",
            "git",
            "status",
        ])
        .unwrap();
        match cli.command {
            Commands::Hook {
                command: HookCommands::Check { agent, command },
            } => {
                assert_eq!(agent, "claude");
                assert_eq!(command, vec!["shadowenv", "exec", "--", "git", "status"]);
            }
            _ => panic!("Expected Hook Check command"),
        }
    }

    #[test]
    fn test_meta_command_list_is_complete() {
        // Verify all meta-commands are in the guard list by checking they parse with valid syntax
        let meta_cmds_that_parse = [
            vec!["rtk", "gain"],
            vec!["rtk", "discover"],
            vec!["rtk", "learn"],
            vec!["rtk", "init"],
            vec!["rtk", "config"],
            vec!["rtk", "proxy", "echo", "hi"],
            vec!["rtk", "run", "-c", "echo hi"],
            vec!["rtk", "hook-audit"],
            vec!["rtk", "cc-economics"],
        ];
        for args in &meta_cmds_that_parse {
            let result = Cli::try_parse_from(args.iter());
            assert!(
                result.is_ok(),
                "Meta-command {:?} should parse successfully",
                args
            );
        }
    }

    #[test]
    fn test_shell_split_simple() {
        assert_eq!(
            shell_split("head -50 file.php"),
            vec!["head", "-50", "file.php"]
        );
    }

    #[test]
    fn test_shell_split_double_quotes() {
        assert_eq!(
            shell_split(r#"git log --format="%H %s""#),
            vec!["git", "log", "--format=%H %s"]
        );
    }

    #[test]
    fn test_shell_split_single_quotes() {
        assert_eq!(
            shell_split("grep -r 'hello world' ."),
            vec!["grep", "-r", "hello world", "."]
        );
    }

    #[test]
    fn test_shell_split_single_word() {
        assert_eq!(shell_split("ls"), vec!["ls"]);
    }

    #[test]
    fn test_shell_split_empty() {
        let result: Vec<String> = shell_split("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_rewrite_clap_multi_args() {
        // This is the bug KuSh reported: `rtk rewrite ls -al` failed because
        // Clap rejected `-al` as an unknown flag. With trailing_var_arg + allow_hyphen_values,
        // multiple args are accepted and joined into a single command string.
        let cases = vec![
            vec!["rtk", "rewrite", "ls", "-al"],
            vec!["rtk", "rewrite", "git", "status"],
            vec!["rtk", "rewrite", "npm", "exec"],
            vec!["rtk", "rewrite", "cargo", "test"],
            vec!["rtk", "rewrite", "du", "-sh", "."],
            vec!["rtk", "rewrite", "head", "-50", "file.txt"],
        ];
        for args in &cases {
            let result = Cli::try_parse_from(args.iter());
            assert!(
                result.is_ok(),
                "rtk rewrite {:?} should parse (was failing before trailing_var_arg fix)",
                &args[2..]
            );
            if let Ok(cli) = result {
                match cli.command {
                    Commands::Rewrite { ref args } => {
                        assert!(args.len() >= 2, "rewrite args should capture all tokens");
                    }
                    _ => panic!("expected Rewrite command"),
                }
            }
        }
    }

    #[test]
    fn test_rewrite_clap_quoted_single_arg() {
        // Quoted form: `rtk rewrite "git status"` — single arg containing spaces
        let result = Cli::try_parse_from(["rtk", "rewrite", "git status"]);
        assert!(result.is_ok());
        if let Ok(cli) = result {
            match cli.command {
                Commands::Rewrite { ref args } => {
                    assert_eq!(args.len(), 1);
                    assert_eq!(args[0], "git status");
                }
                _ => panic!("expected Rewrite command"),
            }
        }
    }

    #[test]
    fn test_merge_filters_with_no_args() {
        let filters = vec![];
        let args = vec!["--depth=0".to_string(), "--no-verbose".to_string()];
        let expected_args = vec!["--depth=0", "--no-verbose"];
        assert_eq!(
            merge_pnpm_args(&filters, false, false, &args),
            expected_args
        );
    }

    #[test]
    fn test_merge_filters_with_args() {
        let filters = vec!["@app1".to_string(), "@app2".to_string()];
        let args = vec![
            "--filter=@app3".to_string(),
            "--depth=0".to_string(),
            "--no-verbose".to_string(),
        ];
        let expected_args = vec![
            "--filter=@app1",
            "--filter=@app2",
            "--filter=@app3",
            "--depth=0",
            "--no-verbose",
        ];
        assert_eq!(
            merge_pnpm_args(&filters, false, false, &args),
            expected_args
        );
    }

    #[test]
    fn test_merge_filters_with_no_args_os() {
        let filters = vec![];
        let args = vec![OsString::from("--depth=0")];
        let expected_args = vec![OsString::from("--depth=0")];
        assert_eq!(
            merge_pnpm_args_os(&filters, false, false, &args),
            expected_args
        );
    }

    #[test]
    fn test_merge_filters_with_args_os() {
        let filters = vec!["@app1".to_string()];
        let args = vec![OsString::from("--depth=0")];
        let expected_args = vec![
            OsString::from("--filter=@app1"),
            OsString::from("--depth=0"),
        ];
        assert_eq!(
            merge_pnpm_args_os(&filters, false, false, &args),
            expected_args
        );
    }

    #[test]
    fn test_merge_recursive_workspace_root_ordering() {
        // Global flags come first, then filters, then the subcommand args —
        // executed as `pnpm <sub> -r -w --filter=@app <args>` (position-independent,
        // verified empirically; see pnpm_global_flags docs).
        let filters = vec!["@app".to_string()];
        let args = vec!["--depth=0".to_string()];
        assert_eq!(
            merge_pnpm_args(&filters, true, true, &args),
            vec!["-r", "-w", "--filter=@app", "--depth=0"]
        );
        // No flags → unchanged from the filters-only behavior.
        assert_eq!(merge_pnpm_args(&[], false, false, &args), vec!["--depth=0"]);
    }

    #[test]
    fn test_validate_pnpm_globals_warns_on_typecheck() {
        let cmd = PnpmCommands::Typecheck { args: vec![] };
        assert!(validate_pnpm_globals(true, false, &cmd).is_some());
        assert!(validate_pnpm_globals(false, true, &cmd).is_some());
        // Both flags at once: single warning naming both.
        let both = validate_pnpm_globals(true, true, &cmd).unwrap();
        assert!(both.contains("-r") && both.contains("-w"));
        assert!(validate_pnpm_globals(false, false, &cmd).is_none());
        // Non-typecheck subcommands forward the flags → no warning.
        let install = PnpmCommands::Install { args: vec![] };
        assert!(validate_pnpm_globals(true, true, &install).is_none());
    }

    #[test]
    fn test_pnpm_subcommand_with_filter() {
        let cli = Cli::try_parse_from([
            "rtk", "pnpm", "--filter", "@app1", "--filter", "@app2", "list", "--filter", "@app3",
            "--filter", "@app4", "--prod",
        ])
        .unwrap();
        match cli.command {
            Commands::Pnpm {
                filter,
                command: PnpmCommands::List { depth, args },
                ..
            } => {
                assert_eq!(depth, 0);
                assert_eq!(filter, vec!["@app1", "@app2"]);
                assert_eq!(
                    args,
                    vec!["--filter", "@app3", "--filter", "@app4", "--prod"]
                );
            }
            _ => panic!("Expected Pnpm List command"),
        }
    }

    #[test]
    fn test_git_push_u_flag_passes_through() {
        let cli = Cli::try_parse_from(["rtk", "git", "push", "-u", "origin", "my-branch"]).unwrap();
        assert!(
            !cli.ultra_compact,
            "-u on git push must NOT be consumed as --ultra-compact"
        );
        match cli.command {
            Commands::Git {
                command: GitCommands::Push { args },
                ..
            } => {
                assert!(
                    args.contains(&"-u".to_string()),
                    "-u must be forwarded to git push, got: {:?}",
                    args
                );
            }
            _ => panic!("Expected Git Push command"),
        }
    }

    #[test]
    fn test_pnpm_subcommand_with_short_filter() {
        // -F is the short form of --filter in pnpm
        let cli =
            Cli::try_parse_from(["rtk", "pnpm", "-F", "@app1", "-F", "@app2", "list"]).unwrap();
        match cli.command {
            Commands::Pnpm { filter, .. } => {
                assert_eq!(filter, vec!["@app1", "@app2"]);
            }
            _ => panic!("Expected Pnpm command"),
        }
    }

    #[test]
    fn test_pnpm_typecheck_without_filters() {
        let cli = Cli::try_parse_from([
            "rtk",
            "pnpm",
            "typecheck",
            "--filter",
            "@app3",
            "--filter",
            "@app4",
        ])
        .unwrap();
        match cli.command {
            Commands::Pnpm {
                filter, command, ..
            } => {
                let warning = validate_pnpm_filters(&filter, &command);

                assert!(filter.is_empty());
                assert!(warning.is_none())
            }
            _ => panic!("Expected Pnpm Build command"),
        }
    }

    #[test]
    fn test_pnpm_typecheck_with_filters() {
        let cli = Cli::try_parse_from([
            "rtk",
            "pnpm",
            "--filter",
            "@app1",
            "--filter",
            "@app2",
            "typecheck",
            "--filter",
            "@app3",
            "--filter",
            "@app4",
        ])
        .unwrap();
        match cli.command {
            Commands::Pnpm {
                filter, command, ..
            } => {
                let warning = validate_pnpm_filters(&filter, &command).unwrap();

                assert_eq!(filter, vec!["@app1", "@app2"]);
                assert_eq!(
                    warning,
                    "[rtk] warning: --filter is not yet supported for pnpm tsc, filters preceding the subcommand will be ignored"
                )
            }
            _ => panic!("Expected Pnpm Build command"),
        }
    }

    #[test]
    #[ignore] // Integration test: requires `cargo build` first
    fn test_broken_pipe_does_not_crash() {
        // A throwaway repo, not whichever one the contributor happens to be sitting in:
        // outside a repo `git log` only errors, and the test stops exercising the pipe.
        let repo = test_isolation::temp_git_repo();

        let mut child = test_isolation::rtk_command()
            .args(["git", "log", "--oneline", "-50"])
            .current_dir(repo.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn rtk");

        // Read one byte then drop stdout to close the pipe.
        let mut stdout = child.stdout.take().unwrap();
        let mut buf = [0u8; 1];
        let _ = std::io::Read::read(&mut stdout, &mut buf);

        let status = child.wait().expect("Failed to wait for rtk");
        let code = status.code().unwrap_or(-1);

        assert_ne!(
            code, 134,
            "rtk crashed with SIGABRT (exit 134) on broken pipe - SIGPIPE handler missing"
        );
    }

    #[test]
    fn test_ultra_compact_long_form_still_works() {
        let cli = Cli::try_parse_from(["rtk", "--ultra-compact", "git", "status"]).unwrap();
        assert!(
            cli.ultra_compact,
            "--ultra-compact long form must still enable ultra-compact mode"
        );
    }

    #[test]
    fn test_npx_unknown_tool_passthrough() {
        // The bug (rtk-ai/rtk#815) was that unknown tools under `rtk npx`
        // were dispatched to `npm` instead of `npx`. At the parse level, the
        // Npx variant must carry all args through unchanged so the dispatch
        // arm can forward them to npx.
        let cli = Cli::try_parse_from(["rtk", "npx", "cowsay", "hello"]).unwrap();
        match cli.command {
            Commands::Npx { args } => {
                assert_eq!(args, vec!["cowsay", "hello"]);
            }
            _ => panic!("Expected Commands::Npx for unknown tool"),
        }
    }

    #[test]
    fn test_init_pi_flag_rejected() {
        // --pi has been removed; --agent pi is the canonical form
        let result = Cli::try_parse_from(["rtk", "init", "--pi"]);
        assert!(result.is_err(), "--pi must be rejected as unknown argument");
    }

    #[test]
    fn test_init_agent_pi_parses() {
        let cli = Cli::try_parse_from(["rtk", "init", "--agent", "pi"]).unwrap();
        match cli.command {
            Commands::Init { agent, .. } => {
                assert_eq!(
                    agent,
                    Some(AgentTarget::Pi),
                    "--agent pi must set Pi variant"
                );
            }
            _ => panic!("Expected Init command"),
        }
    }

    #[test]
    fn test_init_uninstall_agent_pi_parses() {
        let cli = Cli::try_parse_from(["rtk", "init", "--uninstall", "--agent", "pi", "--global"])
            .unwrap();
        match cli.command {
            Commands::Init {
                uninstall,
                agent,
                global,
                ..
            } => {
                assert!(uninstall);
                assert_eq!(agent, Some(AgentTarget::Pi));
                assert!(global);
            }
            _ => panic!("Expected Init command"),
        }
    }

    // --- grep argument routing (clap layer) ---
    //
    // The `Grep` variant funnels the pattern, path, and every native grep/rg flag
    // into one trailing slot so `src/cmds/system/search.rs` parses them with full
    // grep/rg semantics. These tests pin that routing.

    /// Parse `rtk grep …` and return the captured `extra_args`, or `None` if
    /// clap rejects the invocation (e.g. a colliding short option mis-binds).
    fn grep_extra_args(args: &[&str]) -> Option<Vec<String>> {
        match Cli::try_parse_from(args).ok()?.command {
            Commands::Grep { extra_args, .. } => Some(extra_args),
            _ => None,
        }
    }

    #[test]
    fn test_grep_parse_simple_pattern_path() {
        assert_eq!(
            grep_extra_args(&["rtk", "grep", "FOO", "src/"]).unwrap(),
            vec!["FOO", "src/"]
        );
    }

    #[test]
    fn test_grep_parse_combined_short_cluster() {
        // `-rn` is a native grep cluster (recursive + line-numbers); it must
        // reach search.rs intact, not be intercepted by clap.
        assert_eq!(
            grep_extra_args(&["rtk", "grep", "-rn", "FOO", "src/"]).unwrap(),
            vec!["-rn", "FOO", "src/"]
        );
    }

    #[test]
    fn test_grep_parse_value_flag_after_context() {
        // `-A 3` (after-context) — the value must not be stolen by clap.
        assert_eq!(
            grep_extra_args(&["rtk", "grep", "-A", "3", "FOO", "file"]).unwrap(),
            vec!["-A", "3", "FOO", "file"]
        );
    }

    #[test]
    fn test_grep_parse_invert_match_v_not_shadowed() {
        // `-v` (invert-match) must reach grep, not be captured as rtk's
        // top-level verbose flag.
        assert_eq!(
            grep_extra_args(&["rtk", "grep", "-v", "FOO", "file"]).unwrap(),
            vec!["-v", "FOO", "file"]
        );
    }

    #[test]
    fn test_grep_parse_alternation_pattern_intact() {
        // Extended-regex alternation must pass through untouched.
        assert_eq!(
            grep_extra_args(&["rtk", "grep", "-nE", "a|b", "file"]).unwrap(),
            vec!["-nE", "a|b", "file"]
        );
    }

    #[test]
    fn test_grep_parse_files_with_matches_l() {
        // Native grep `-l` (files-with-matches). Must parse and pass `-l` through.
        assert_eq!(
            grep_extra_args(&["rtk", "grep", "-l", "FOO", "src/"]).unwrap(),
            vec!["-l", "FOO", "src/"]
        );
    }

    #[test]
    fn test_grep_parse_type_t_forwarded() {
        // `-t TYPE` is rg-only and must reach the engine: `rtk rg` filters by
        // type, `rtk grep` surfaces grep's own `invalid option -- 't'`.
        assert_eq!(
            grep_extra_args(&["rtk", "grep", "-t", "rust", "FOO", "src/"]).unwrap(),
            vec!["-t", "rust", "FOO", "src/"]
        );
    }

    #[test]
    fn test_grep_file_type_option_is_gone() {
        // `--file-type` was removed, not merely un-shorted: an unknown long flag
        // routes to extra_args and is the engine's problem. Re-adding the option
        // would bind it here and cost extra_args these two tokens.
        assert_eq!(
            grep_extra_args(&["rtk", "grep", "--file-type", "rust", "FOO", "src/"]).unwrap(),
            vec!["--file-type", "rust", "FOO", "src/"]
        );
    }

    /// Parse `rtk grep …` into the full `Grep` field set for inspection.
    fn parse_grep(args: &[&str]) -> Result<(usize, usize, bool, Vec<String>), clap::Error> {
        match Cli::try_parse_from(args)?.command {
            Commands::Grep {
                max_len,
                max,
                context_only,
                extra_args,
            } => Ok((max_len, max, context_only, extra_args)),
            _ => unreachable!("parsed a grep command"),
        }
    }

    #[test]
    fn test_grep_long_options_bind_before_pattern() {
        // Long-only knobs must still bind when placed before the pattern, with
        // only the pattern/path landing in extra_args.
        let (max_len, max, context_only, extra_args) = parse_grep(&[
            "rtk",
            "grep",
            "--max-len",
            "40",
            "--max",
            "5",
            "--context-only",
            "FOO",
            "path",
        ])
        .unwrap();
        assert_eq!(max_len, 40);
        assert_eq!(max, 5);
        assert!(context_only);
        assert_eq!(extra_args, vec!["FOO", "path"]);
    }

    #[test]
    fn test_grep_options_after_pattern_stay_in_extra_args() {
        // Once the pattern starts the trailing slot, later tokens — even rtk's
        // own `--max-len` — pass through verbatim rather than binding as options.
        // The mechanism is `allow_hyphen_values`, not `trailing_var_arg`.
        let (max_len, _, _, extra_args) =
            parse_grep(&["rtk", "grep", "FOO", "--max-len", "40"]).unwrap();
        assert_eq!(
            max_len, 80,
            "option after pattern must NOT bind (default kept)"
        );
        assert_eq!(extra_args, vec!["FOO", "--max-len", "40"]);
    }

    #[test]
    fn test_grep_version_routes_to_extra_args() {
        // `--version` is not a subcommand flag (version isn't propagated), so it
        // lands in extra_args, and search::run's --version/--help check execs it
        // against the engine unfiltered.
        assert_eq!(
            grep_extra_args(&["rtk", "grep", "--version"]).unwrap(),
            vec!["--version"]
        );
    }

    #[test]
    fn test_grep_double_dash_consumed_by_clap() {
        // clap strips the `--` separator before populating trailing_var_arg.
        // core::args_utils::restore_double_dash re-inserts it at runtime — this
        // test pins the premise that helper depends on.
        let (_, _, _, extra_args) = parse_grep(&["rtk", "grep", "--", "-v", "file"]).unwrap();
        assert_eq!(extra_args, vec!["-v", "file"], "clap must consume the --");
    }

    #[test]
    fn test_bun_build_parses_to_arg_vector() {
        let cli = Cli::try_parse_from(["rtk", "bun", "build", "--outdir", "dist"]).unwrap();
        match cli.command {
            Commands::Bun {
                command: BunCommands::Build { args },
            } => assert_eq!(args, vec!["--outdir", "dist"]),
            _ => panic!("Expected Bun Build command"),
        }
    }

    #[test]
    fn test_bun_x_parses_to_arg_vector() {
        let cli = Cli::try_parse_from(["rtk", "bun", "x", "tsc", "--noEmit"]).unwrap();
        match cli.command {
            Commands::Bun {
                command: BunCommands::X { args },
            } => assert_eq!(args, vec!["tsc", "--noEmit"]),
            _ => panic!("Expected Bun X command"),
        }
    }

    #[test]
    fn test_bun_pm_ls_parses_to_typed_ls() {
        let cli = Cli::try_parse_from(["rtk", "bun", "pm", "ls"]).unwrap();
        match cli.command {
            Commands::Bun {
                command:
                    BunCommands::Pm {
                        command: BunPmCommands::Ls { args },
                    },
            } => assert!(args.is_empty()),
            _ => panic!("Expected Bun Pm Ls command"),
        }
    }

    #[test]
    fn test_bun_pm_other_passes_through() {
        let cli = Cli::try_parse_from(["rtk", "bun", "pm", "cache", "rm"]).unwrap();
        match cli.command {
            Commands::Bun {
                command:
                    BunCommands::Pm {
                        command: BunPmCommands::Other(args),
                    },
            } => assert_eq!(args, vec![OsString::from("cache"), OsString::from("rm")]),
            _ => panic!("Expected Bun Pm Other passthrough"),
        }
    }

    #[test]
    fn test_deno_compile_parses_to_arg_vector() {
        let cli = Cli::try_parse_from(["rtk", "deno", "compile", "main.ts"]).unwrap();
        match cli.command {
            Commands::Deno {
                command: DenoCommands::Compile { args },
            } => assert_eq!(args, vec!["main.ts"]),
            _ => panic!("Expected Deno Compile command"),
        }
    }
    std::process::exit(rtk::entry());
}
