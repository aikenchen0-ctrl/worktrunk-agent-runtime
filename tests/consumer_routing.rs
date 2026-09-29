use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    state: PathBuf,
    provider: PathBuf,
    buyer: PathBuf,
    seller: PathBuf,
    buyer_worktree: PathBuf,
}

fn isolated_command(program: &str, cwd: &Path, root: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("empty-git-config"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_CONFIG_COUNT");
    command
}

fn describe(output: &Output) -> String {
    format!(
        "退出状态：{}；标准输出：{}；标准错误：{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn success(output: Output) -> Output {
    assert!(output.status.success(), "命令应成功：{}", describe(&output));
    output
}

fn rejected(output: Output, expected: &str) {
    assert!(
        !output.status.success(),
        "命令应拒绝：{}",
        describe(&output)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "错误原因应包含 {expected}：{}",
        describe(&output)
    );
}

fn decoded(output: Output) -> Value {
    let output = success(output);
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("命令未返回合法 JSON：{error}；{}", describe(&output)))
}

fn write_file(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("测试文件应有父目录")).expect("创建测试目录失败");
    fs::write(path, text).expect("写入测试文件失败");
}

fn write_json(path: &Path, value: &Value) {
    write_file(
        path,
        &serde_json::to_string_pretty(value).expect("序列化测试数据失败"),
    );
}

fn git(root: &Path, cwd: &Path, args: &[&str]) -> Output {
    let mut command = isolated_command("git", cwd, root);
    command.args([
        "-c",
        "user.name=Routing Test",
        "-c",
        "user.email=routing-test@example.invalid",
        "-c",
        "commit.gpgSign=false",
    ]);
    command.args(args);
    success(command.output().expect("执行测试 Git 命令失败"))
}

fn commit(root: &Path, repository: &Path) {
    git(root, repository, &["add", "."]);
    git(root, repository, &["commit", "-m", "routing fixture"]);
}

fn initialize_repository(root: &Path, repository: &Path, module: &str) {
    fs::create_dir_all(repository).expect("创建测试仓库失败");
    git(root, repository, &["init", "-b", "main"]);
    write_file(
        &repository.join("settings.gradle.kts"),
        &format!("rootProject.name = \"routing-fixture\"\ninclude(\":{module}\")\n"),
    );
    write_file(
        &repository.join(module).join("build.gradle.kts"),
        "plugins {}\n",
    );
    write_file(
        &repository.join(module).join("src/main/kotlin/Contract.kt"),
        "package routing\npublic interface Contract { fun value(): String }\n",
    );
    commit(root, repository);
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("agentctl-routing-")
            .tempdir()
            .expect("创建隔离测试目录失败");
        let provider = root.path().join("provider");
        let buyer = root.path().join("buyer");
        let seller = root.path().join("seller");
        let buyer_worktree = root.path().join("buyer-worktree");
        let state = root.path().join("state");
        initialize_repository(root.path(), &provider, "provider");
        initialize_repository(root.path(), &buyer, "app");
        initialize_repository(root.path(), &seller, "app");
        write_file(
            &provider.join("config/dependents.yml"),
            "provider: provider\nconsumers:\n  - buyer-app\n  - seller-app\n",
        );
        commit(root.path(), &provider);
        git(
            root.path(),
            &buyer,
            &[
                "worktree",
                "add",
                "-b",
                "routing-validation",
                buyer_worktree.to_str().expect("测试路径应为 UTF-8"),
            ],
        );
        let fixture = Self {
            root,
            state,
            provider,
            buyer,
            seller,
            buyer_worktree,
        };
        // 所有 CLI 子进程共用本用例的独立状态目录，不读取实际工作状态。
        for repository in [&fixture.provider, &fixture.buyer, &fixture.seller] {
            success(fixture.cli(repository, &["prepare", "--allow-primary"]));
        }
        success(fixture.cli(&fixture.buyer_worktree, &["prepare"]));
        fixture
    }

    fn cli(&self, cwd: &Path, args: &[&str]) -> Output {
        isolated_command(env!("CARGO_BIN_EXE_agentctl"), cwd, self.root.path())
            .env("AGENT_RUNTIME_ROOT", &self.state)
            .env("AGENT_ID", "routing-cli")
            .args(args)
            .output()
            .expect("执行 agentctl 测试命令失败")
    }

    fn routes(&self) -> Value {
        json!({
            "buyer-app": {"repository": self.buyer_worktree, "project": ":app"},
            "seller-app": {"repository": self.seller, "project": ":app"}
        })
    }

    fn create_event(&self, routes: Option<&Value>) -> Output {
        let path = self.root.path().join("consumer-targets.json");
        let mut args = vec![
            "contract-event",
            "--provider",
            "provider",
            "--base",
            "1.0.0",
            "--module",
            "provider",
            "--format",
            "json",
        ];
        if let Some(routes) = routes {
            write_json(&path, routes);
            args.extend([
                "--consumer-targets",
                path.to_str().expect("测试路径应为 UTF-8"),
            ]);
        }
        self.cli(&self.provider, &args)
    }

    fn inbox(&self, repository: &Path, consumer: &str) -> Value {
        decoded(self.cli(
            repository,
            &["event-inbox", "--consumer", consumer, "--format", "json"],
        ))
    }

    fn acknowledge(&self, repository: &Path, id: &str, consumer: &str, status: &str) -> Output {
        self.cli(
            repository,
            &[
                "event-ack",
                "--event",
                id,
                "--consumer",
                consumer,
                "--status",
                status,
                "--format",
                "json",
            ],
        )
    }

    fn import_event(&self, event: &Value) -> Output {
        let path = self.root.path().join("import-event.json");
        write_json(&path, event);
        self.cli(
            &self.provider,
            &[
                "event-import",
                "--file",
                path.to_str().expect("测试路径应为 UTF-8"),
                "--format",
                "json",
            ],
        )
    }

    fn assert_event_absent(&self, id: &str) {
        assert!(
            !self
                .state
                .join("events")
                .join(format!("{id}.json"))
                .exists(),
            "被拒绝的事件不得写入状态目录"
        );
    }

    fn assert_no_events(&self) {
        let directory = self.state.join("events");
        if directory.exists() {
            assert!(
                fs::read_dir(directory)
                    .expect("读取事件目录失败")
                    .all(|entry| {
                        entry
                            .expect("读取事件文件失败")
                            .path()
                            .extension()
                            .is_none_or(|ext| ext != "json")
                    }),
                "失败的候选创建不得留下事件文件"
            );
        }
    }
}

fn assert_repository(actual: &Value, expected: &Path) {
    assert_eq!(
        fs::canonicalize(actual.as_str().expect("仓库字段应为字符串")).expect("解析事件仓库失败"),
        fs::canonicalize(expected).expect("解析预期仓库失败"),
        "路由应指向正确的 Git 主仓库"
    );
}

#[test]
fn explicit_routes_complete_yaml_and_isolate_same_named_projects() {
    let fixture = Fixture::new();
    let event = decoded(fixture.create_event(Some(&fixture.routes())));
    let id = event["event_id"].as_str().expect("事件应有标识");
    assert_eq!(
        event["affected_consumers"],
        json!(["buyer-app", "seller-app"])
    );
    assert_eq!(event["consumer_targets"]["buyer-app"]["project"], ":app");
    assert_eq!(event["consumer_targets"]["seller-app"]["project"], ":app");
    assert_repository(
        &event["consumer_targets"]["buyer-app"]["repository"],
        &fixture.buyer,
    );
    assert_repository(
        &event["consumer_targets"]["seller-app"]["repository"],
        &fixture.seller,
    );

    for repository in [&fixture.buyer, &fixture.buyer_worktree] {
        let inbox = fixture.inbox(repository, "buyer-app");
        assert_eq!(inbox.as_array().expect("收件箱应为数组").len(), 1);
        assert_eq!(inbox[0]["event_id"], id);
        assert_eq!(fixture.inbox(repository, "seller-app"), json!([]));
    }
    assert_eq!(
        fixture.inbox(&fixture.seller, "seller-app")[0]["event_id"],
        id
    );
    assert_eq!(fixture.inbox(&fixture.seller, "buyer-app"), json!([]));
    assert_eq!(fixture.inbox(&fixture.provider, "buyer-app"), json!([]));
    assert_eq!(fixture.inbox(&fixture.provider, "seller-app"), json!([]));

    let ack_path = fixture
        .state
        .join("events")
        .join(id)
        .join("acks/buyer-app.json");
    for status in ["received", "validation_started"] {
        rejected(
            fixture.acknowledge(&fixture.seller, id, "buyer-app", status),
            "拒绝跨仓确认",
        );
        assert!(!ack_path.exists(), "错误仓库不得创建消费者回执");
    }
    let received =
        decoded(fixture.acknowledge(&fixture.buyer_worktree, id, "buyer-app", "received"));
    assert_eq!(received["consumer_project"], ":app");
    assert_repository(&received["repository"], &fixture.buyer);
    let before = fs::read(&ack_path).expect("正确仓库应创建回执");
    for repository in [&fixture.seller, &fixture.provider] {
        for status in ["received", "validation_started"] {
            rejected(
                fixture.acknowledge(repository, id, "buyer-app", status),
                "拒绝跨仓确认",
            );
            assert_eq!(
                fs::read(&ack_path).expect("回执应仍存在"),
                before,
                "拒绝操作不得覆盖回执"
            );
        }
    }
    let started = decoded(fixture.acknowledge(
        &fixture.buyer_worktree,
        id,
        "buyer-app",
        "validation_started",
    ));
    assert_eq!(started["status"], "validation_started");
    let before = fs::read(&ack_path).expect("验证租约应已写入");
    rejected(
        fixture.acknowledge(&fixture.seller, id, "buyer-app", "validation_started"),
        "拒绝跨仓确认",
    );
    assert_eq!(fs::read(&ack_path).expect("验证租约应仍存在"), before);
    let seller = decoded(fixture.acknowledge(&fixture.seller, id, "seller-app", "received"));
    assert_eq!(seller["consumer"], "seller-app");
    assert_eq!(fs::read(&ack_path).expect("买方回执应仍存在"), before);
}

#[test]
fn declared_consumers_require_complete_explicit_routes() {
    let fixture = Fixture::new();
    rejected(fixture.create_event(None), "--consumer-targets");
    fixture.assert_no_events();
    let mut partial = fixture.routes();
    partial
        .as_object_mut()
        .expect("路由应为对象")
        .remove("seller-app");
    rejected(fixture.create_event(Some(&partial)), "seller-app");
    fixture.assert_no_events();
}

#[test]
fn case_only_alias_collisions_are_rejected_when_creating_and_importing() {
    let fixture = Fixture::new();
    let mut routes = fixture.routes();
    routes["Buyer-app"] = json!({"repository": fixture.seller, "project": ":app"});
    rejected(fixture.create_event(Some(&routes)), "大小写");
    fixture.assert_no_events();

    let mut event = decoded(fixture.create_event(Some(&fixture.routes())));
    event["event_id"] = json!("case-collision");
    event["affected_consumers"] = json!(["Buyer-app", "buyer-app"]);
    event["consumer_targets"] = json!({
        "Buyer-app": {"repository": fixture.seller, "project": ":app"},
        "buyer-app": {"repository": fixture.buyer, "project": ":app"}
    });
    rejected(fixture.import_event(&event), "大小写");
    fixture.assert_event_absent("case-collision");
    event["event_id"] = json!("legacy-case-collision");
    event
        .as_object_mut()
        .expect("事件应为对象")
        .remove("routing_schema_version");
    event
        .as_object_mut()
        .expect("事件应为对象")
        .remove("consumer_targets");
    rejected(fixture.import_event(&event), "大小写");
    fixture.assert_event_absent("legacy-case-collision");
}

#[test]
fn malformed_versioned_routes_never_fall_back_to_legacy_routing() {
    let fixture = Fixture::new();
    let original = decoded(fixture.create_event(Some(&fixture.routes())));
    for (id, version) in [
        ("missing-routes-v1", json!(1)),
        ("missing-routes-v2", json!(2)),
        ("missing-routes-null", Value::Null),
    ] {
        let mut event = original.clone();
        event["event_id"] = json!(id);
        event["routing_schema_version"] = version;
        event
            .as_object_mut()
            .expect("事件应为对象")
            .remove("consumer_targets");
        let output = fixture.import_event(&event);
        assert!(
            !output.status.success(),
            "缺少完整路由的新版事件必须拒绝：{}",
            describe(&output)
        );
        fixture.assert_event_absent(id);
    }
    let mut partial = original.clone();
    partial["event_id"] = json!("partial-routes");
    partial["consumer_targets"]
        .as_object_mut()
        .expect("路由应为对象")
        .remove("seller-app");
    rejected(fixture.import_event(&partial), "逐一对应");
    fixture.assert_event_absent("partial-routes");
    let mut no_version = original;
    no_version["event_id"] = json!("missing-route-version");
    no_version
        .as_object_mut()
        .expect("事件应为对象")
        .remove("routing_schema_version");
    rejected(fixture.import_event(&no_version), "路由版本不支持");
    fixture.assert_event_absent("missing-route-version");
}

#[test]
fn legacy_events_remain_scoped_to_the_provider_repository() {
    let fixture = Fixture::new();
    let mut event = decoded(fixture.create_event(Some(&fixture.routes())));
    event["event_id"] = json!("legacy-event");
    event["affected_consumers"] = json!(["app"]);
    event
        .as_object_mut()
        .expect("事件应为对象")
        .remove("consumer_targets");
    event
        .as_object_mut()
        .expect("事件应为对象")
        .remove("routing_schema_version");
    decoded(fixture.import_event(&event));
    assert_eq!(
        fixture.inbox(&fixture.provider, "app")[0]["event_id"],
        "legacy-event"
    );
    for repository in [&fixture.buyer, &fixture.buyer_worktree, &fixture.seller] {
        assert_eq!(fixture.inbox(repository, "app"), json!([]));
        rejected(
            fixture.acknowledge(repository, "legacy-event", "app", "received"),
            "拒绝跨仓确认",
        );
    }
    let ack = decoded(fixture.acknowledge(&fixture.provider, "legacy-event", "app", "received"));
    assert_eq!(ack["consumer_project"], ":app");
    assert_repository(&ack["repository"], &fixture.provider);
}

#[test]
fn separate_git_directories_cannot_become_consumer_repository_identities() {
    let fixture = Fixture::new();
    let external = fixture.root.path().join("external");
    let git_directory = fixture.root.path().join("git-storage/external.git");
    fs::create_dir_all(&external).expect("创建外置 Git 测试目录失败");
    fs::create_dir_all(git_directory.parent().expect("Git 管理目录应有父目录"))
        .expect("创建外置 Git 管理目录失败");
    git(
        fixture.root.path(),
        &external,
        &[
            "init",
            "-b",
            "main",
            "--separate-git-dir",
            git_directory.to_str().expect("测试路径应为 UTF-8"),
        ],
    );
    write_file(&external.join("README.md"), "external git fixture\n");
    commit(fixture.root.path(), &external);
    rejected(
        fixture.cli(&external, &["prepare", "--allow-primary"]),
        "外置 Git 管理目录",
    );
    let mut routes = fixture.routes();
    routes["buyer-app"]["repository"] = json!(external);
    rejected(fixture.create_event(Some(&routes)), "外置 Git 管理目录");
    fixture.assert_no_events();
}

#[test]
fn explicit_same_repository_routes_can_correct_nested_project_paths() {
    let fixture = Fixture::new();
    write_file(
        &fixture.provider.join("feature/app/build.gradle.kts"),
        "dependencies { implementation(project(\":provider\")) }\n",
    );
    write_file(
        &fixture.provider.join("settings.gradle.kts"),
        "rootProject.name = \"routing-fixture\"\ninclude(\":provider\", \":feature:app\")\n",
    );
    commit(fixture.root.path(), &fixture.provider);
    let mut routes = fixture.routes();
    routes["app"] = json!({"repository": fixture.provider, "project": ":feature:app"});
    let event = decoded(fixture.create_event(Some(&routes)));
    let id = event["event_id"].as_str().expect("事件应有标识");
    assert_eq!(
        event["affected_consumers"],
        json!(["app", "buyer-app", "seller-app"])
    );
    assert_eq!(event["consumer_targets"]["app"]["project"], ":feature:app");
    assert_repository(
        &event["consumer_targets"]["app"]["repository"],
        &fixture.provider,
    );
    assert_eq!(fixture.inbox(&fixture.provider, "app")[0]["event_id"], id);
    assert_eq!(fixture.inbox(&fixture.buyer, "app"), json!([]));
    let ack = decoded(fixture.acknowledge(&fixture.provider, id, "app", "received"));
    assert_eq!(ack["consumer_project"], ":feature:app");
}
