use std::process::Command;
use std::{fs, path::Path};
use tempfile::tempdir;

#[test]
fn invalid_prepare_does_not_initialize_runtime_or_discover_git() {
    let temp = tempdir().unwrap();
    let state = temp.path().join("state");
    let output = Command::new(env!("CARGO_BIN_EXE_agentctl"))
        .current_dir(temp.path())
        .env("AGENT_RUNTIME_ROOT", &state)
        .args(["prepare", "--allow-primary", "--unexpected"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!state.exists());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("未识别参数"));
    assert!(!error.contains("git rev-parse"));
}

#[test]
fn all_subcommand_help_works_outside_a_repository_without_writes() {
    let temp = tempdir().unwrap();
    let state = temp.path().join("state");
    for command in [
        "prepare",
        "doctor",
        "cleanup",
        "install-cache",
        "run-gradle",
        "event-import",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_agentctl"))
            .current_dir(temp.path())
            .env("AGENT_RUNTIME_ROOT", &state)
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{command}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("agentctl"));
    }
    assert!(!state.exists());
}

#[test]
fn cleanup_selectors_apply_worktree_scope_without_redundant_flag() {
    for selector in ["--worktree-path", "--worktree-branch"] {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let worktree = temp.path().join("linked");
        let state = temp.path().join("runtime");
        fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(&repo)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", temp.path().join("no-global-config"))
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_COMMON_DIR")
                .env_remove("GIT_INDEX_FILE")
                .args([
                    "-c",
                    "user.name=Runtime Test",
                    "-c",
                    "user.email=runtime@example.invalid",
                    "-c",
                    "commit.gpgSign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        git(&["commit", "--allow-empty", "-m", "fixture"]);
        git(&[
            "worktree",
            "add",
            "-b",
            "feature",
            worktree.to_str().unwrap(),
        ]);
        let ctl = |cwd: &Path, args: &[&str]| {
            let output = Command::new(env!("CARGO_BIN_EXE_agentctl"))
                .current_dir(cwd)
                .env("AGENT_RUNTIME_ROOT", &state)
                .env_remove("AGENT_ID")
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        let prepared = ctl(&worktree, &["prepare"]);
        let metadata: serde_json::Value = serde_json::from_slice(&prepared.stdout).unwrap();
        let root = Path::new(metadata["paths"]["worktree_root"].as_str().unwrap());
        let target = if selector == "--worktree-path" {
            worktree.to_str().unwrap()
        } else {
            "feature"
        };
        ctl(&repo, &["cleanup", selector, target]);
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("worktree.json")).unwrap()).unwrap();
        assert_eq!(record["status"], "removed");
        assert!(worktree.exists(), "清理不能删除业务 Worktree");
        assert!(root.join("gradle-user-home").exists(), "默认必须保留缓存");
    }
}
