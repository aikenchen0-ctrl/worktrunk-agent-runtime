//! 候选事件按仓库和 Gradle 项目定位消费者，模块名仅作为事件内的唯一标识。
use super::*;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConsumerTarget {
    pub repository: String,
    pub project: String,
}

fn valid_project(value: &str) -> bool {
    value == ":"
        || (value.starts_with(':')
            && value[1..].split(':').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            }))
}

pub(super) fn task_belongs_to_project(project: &str, task: &str) -> bool {
    let prefix = if project == ":" {
        ":".to_string()
    } else {
        format!("{project}:")
    };
    valid_project(project)
        && task.strip_prefix(&prefix).is_some_and(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
}

fn repository_key(value: &str) -> String {
    let path = fs::canonicalize(value).unwrap_or_else(|_| PathBuf::from(value));
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text
        .strip_prefix("//?/")
        .unwrap_or(&text)
        .trim_end_matches('/');
    if cfg!(windows) {
        text.to_ascii_lowercase()
    } else {
        text.to_string()
    }
}

pub(super) fn candidate_targets(
    metadata: &Metadata,
    consumers: &BTreeSet<String>,
    declared_consumers: &BTreeSet<String>,
    extra_file: Option<&str>,
) -> Result<BTreeMap<String, ConsumerTarget>> {
    let mut targets = consumers
        .iter()
        .map(|consumer| {
            (
                consumer.clone(),
                ConsumerTarget {
                    repository: metadata.repository.clone(),
                    project: format!(":{consumer}"),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    if let Some(file) = extra_file {
        let extra: BTreeMap<String, ConsumerTarget> =
            read_json(Path::new(file)).with_context(|| format!("读取消费者路由失败：{file}"))?;
        for (consumer, mut target) in extra {
            if !valid_agent_id(&consumer)
                || !valid_project(&target.project)
                || !Path::new(&target.repository).is_absolute()
            {
                bail!("消费者路由必须包含合法标识、绝对仓库路径和 Gradle 项目：{consumer}");
            }
            // 使用 Git 确认归属，允许传入同一仓库的任意 Worktree 路径。
            let runtime = Runtime {
                cwd: PathBuf::from(&target.repository),
                state_root: PathBuf::new(),
                agent_id: None,
            };
            target.repository = runtime.git_info()?.repository.display().to_string();
            if targets.get(&consumer).is_some_and(|existing| {
                repository_key(&existing.repository) != repository_key(&target.repository)
            }) {
                bail!("消费者标识 {consumer} 已指向本仓库模块；跨仓同名模块必须使用不同标识");
            }
            targets.insert(consumer, target);
        }
    }
    for consumer in declared_consumers {
        if !targets.contains_key(consumer) {
            bail!(
                "登记消费者 {consumer} 未在本仓依赖图中发现；必须通过 --consumer-targets 指定仓库和 Gradle 项目"
            );
        }
    }
    let names = targets
        .keys()
        .map(|name| name.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    if names.len() != targets.len() {
        bail!("消费者标识不能仅大小写不同，避免 Windows 回执文件冲突");
    }
    Ok(targets)
}

pub(super) fn consumer_target(event: &Value, consumer: &str) -> Result<ConsumerTarget> {
    if !event["affected_consumers"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(consumer)))
    {
        bail!("消费者 {consumer} 不在事件的受影响列表中");
    }
    let target = if event.get("consumer_targets").is_some()
        || event.get("routing_schema_version").is_some()
    {
        if event["routing_schema_version"] != 1 {
            bail!("候选事件的消费者路由版本不支持");
        }
        serde_json::from_value::<ConsumerTarget>(event["consumer_targets"][consumer].clone())
            .with_context(|| format!("消费者 {consumer} 缺少完整路由"))?
    } else {
        // 旧事件只能在提供方原仓库内验证，禁止按同名模块猜测跨仓归属。
        ConsumerTarget {
            repository: event["repository"].as_str().unwrap_or_default().to_string(),
            project: format!(":{consumer}"),
        }
    };
    if target.repository.is_empty() || !valid_project(&target.project) {
        bail!("消费者 {consumer} 的仓库或 Gradle 项目无效");
    }
    Ok(target)
}

pub(super) fn require_consumer_repository(
    event: &Value,
    consumer: &str,
    repository: &str,
) -> Result<ConsumerTarget> {
    validate_event_routes(event)?;
    let target = consumer_target(event, consumer)?;
    if repository_key(&target.repository) != repository_key(repository) {
        bail!(
            "消费者 {consumer} 属于仓库 {}，当前仓库为 {repository}；拒绝跨仓确认",
            target.repository
        );
    }
    Ok(target)
}

pub(super) fn validate_event_routes(event: &Value) -> Result<()> {
    let consumers = event["affected_consumers"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("候选事件缺少消费者列表"))?;
    let mut normalized = BTreeSet::new();
    for consumer in consumers {
        let name = consumer
            .as_str()
            .filter(|name| valid_agent_id(name))
            .ok_or_else(|| anyhow::anyhow!("消费者标识无效"))?;
        if !normalized.insert(name.to_ascii_lowercase()) {
            bail!("消费者标识重复或仅大小写不同，拒绝使用可能冲突的回执文件");
        }
    }
    if event.get("consumer_targets").is_none() && event.get("routing_schema_version").is_none() {
        return Ok(());
    }
    if event["routing_schema_version"] != 1 {
        bail!("候选事件的消费者路由版本不支持");
    }
    let routes = event["consumer_targets"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("consumer_targets 必须是对象"))?;
    let names = consumers
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    if names.len() != consumers.len()
        || routes.len() != names.len()
        || routes.keys().any(|key| !names.contains(key.as_str()))
    {
        bail!("消费者路由必须与受影响列表逐一对应，且不能重复");
    }
    for consumer in names {
        let target = consumer_target(event, consumer)?;
        if !Path::new(&target.repository).is_absolute() {
            bail!("消费者 {consumer} 的仓库路由必须是绝对路径");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_same_name_consumer_is_restricted_to_provider_repository() {
        let event = json!({"repository":"repo-a", "affected_consumers":["app"]});
        assert!(require_consumer_repository(&event, "app", "repo-a").is_ok());
        assert!(require_consumer_repository(&event, "app", "repo-b").is_err());
        let missing = json!({"affected_consumers":["app"]});
        assert!(require_consumer_repository(&missing, "app", "repo-a").is_err());
    }

    #[test]
    fn explicit_cross_repository_aliases_keep_same_project_separate() {
        let root = tempfile::tempdir().unwrap();
        let buyer = root.path().join("buyer").display().to_string();
        let seller = root.path().join("seller").display().to_string();
        let event = json!({"routing_schema_version":1,
        "affected_consumers":["buyer-app","seller-app"], "consumer_targets":{
            "buyer-app":{"repository":buyer, "project":":app"},
            "seller-app":{"repository":seller, "project":":app"}
        }});
        assert!(require_consumer_repository(&event, "buyer-app", &buyer).is_ok());
        assert!(require_consumer_repository(&event, "buyer-app", &seller).is_err());
        assert!(require_consumer_repository(&event, "seller-app", &seller).is_ok());
        assert!(require_consumer_repository(&event, "app", &buyer).is_err());
    }

    #[test]
    fn malformed_explicit_routes_never_fall_back_to_legacy_repository() {
        let event = json!({"repository":"repo-a", "routing_schema_version":1,
            "affected_consumers":["app"], "consumer_targets":{}});
        assert!(require_consumer_repository(&event, "app", "repo-a").is_err());
        assert!(validate_event_routes(&event).is_err());
        assert!(valid_project(":"));
        assert!(valid_project(":feature:app"));
        assert!(!valid_project(":feature::app"));
        assert!(!valid_project(":app --offline"));
        for version in [json!(1), json!(2), Value::Null] {
            let missing = json!({"repository":"repo-a", "routing_schema_version":version,
                "affected_consumers":["app"]});
            assert!(require_consumer_repository(&missing, "app", "repo-a").is_err());
            assert!(validate_event_routes(&missing).is_err());
        }
    }

    #[test]
    fn task_paths_support_root_and_reject_descendant_projects() {
        assert!(task_belongs_to_project(":", ":test"));
        assert!(task_belongs_to_project(":app", ":app:testDebugUnitTest"));
        assert!(!task_belongs_to_project(":app", ":app:child:test"));
        assert!(!task_belongs_to_project(":", ":app:test"));
        assert!(!task_belongs_to_project(":", "::test"));
    }

    #[test]
    fn consumer_aliases_cannot_collide_on_case_insensitive_filesystems() {
        let event = json!({"repository":"repo", "affected_consumers":["Buyer-app","buyer-app"]});
        assert!(validate_event_routes(&event).is_err());
        assert!(require_consumer_repository(&event, "Buyer-app", "repo").is_err());
    }
}
