//! docker-compose services: their environment, env files, links and build directories.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::Value;

use super::{End, Infra, Lines, UnitKind, Var, host_variable, hosts, normalize, read_file, scalar};
use crate::arch::types::Evidence;
use crate::modules::dir_of;

/// One service of a compose file.
#[derive(Debug)]
pub(super) struct Service {
    name: String,
    /// The directory it builds from, as a repository path.
    build: Option<String>,
    vars: Vec<Var>,
    /// Its env files, as repository paths.
    env_files: Vec<String>,
    /// Host aliases its links give, with the service each names.
    aliases: Vec<(String, String)>,
}

/// The services of the compose file `file`, or `None` when it does not parse or has none.
/// Variables from env files are left for [`with_env_files`] to add.
pub(super) fn parse(file: &str, text: &str) -> Option<Vec<Service>> {
    let parsed: Value = serde_yaml_ng::from_str(text).ok()?;
    let services = parsed.get("services")?.as_object()?;
    let dir = dir_of(file);
    let lines = Lines::new(text);
    let start = lines.key(1, "services");
    let services = services.iter().map(|(name, spec)| {
        let build = match spec.get("build") {
            Some(Value::String(context)) => Some(context.as_str()),
            Some(build) => build.get("context").and_then(Value::as_str),
            None => None,
        };
        let at = lines.key(start, name);
        Service {
            name: name.clone(),
            build: build.and_then(|context| normalize(dir, context)),
            vars: environment(file, &lines, at, spec.get("environment")),
            env_files: (strings(spec.get("env_file"), "path").iter())
                .filter_map(|path| normalize(dir, path))
                .collect(),
            aliases: (strings(spec.get("links"), "").iter())
                .filter_map(|link| link.split_once(':'))
                .map(|(service, alias)| (alias.to_owned(), service.to_owned()))
                .collect(),
        }
    });
    Some(services.collect())
}

/// `services` with the variables of their env files, each file read once per `cache`.
pub(super) fn with_env_files(
    root: &Path,
    mut services: Vec<Service>,
    cache: &mut HashMap<String, Vec<Var>>,
) -> Vec<Service> {
    for service in &mut services {
        for env in &service.env_files {
            let vars = cache.entry(env.clone()).or_insert_with(|| {
                read_file(root, env).map_or_else(Vec::new, |text| env_file(env, &text))
            });
            service.vars.extend(vars.iter().cloned());
        }
    }
    services
}

/// A string or a list of them; with `key`, list entries may also be maps holding it.
fn strings(value: Option<&Value>, key: &str) -> Vec<String> {
    match value {
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Array(list)) => (list.iter())
            .filter_map(|v| v.as_str().or_else(|| v.get(key)?.as_str()))
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// A service's `environment`, as a map or as a list of `KEY=value`, whose service header is on
/// line `at`.
fn environment(file: &str, lines: &Lines<'_>, at: usize, env: Option<&Value>) -> Vec<Var> {
    let pairs: Vec<(String, String)> = match env {
        Some(Value::Object(map)) => (map.iter())
            .filter_map(|(k, v)| Some((k.clone(), scalar(v)?)))
            .collect(),
        Some(Value::Array(list)) => (list.iter().filter_map(Value::as_str))
            .filter_map(|item| item.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
        _ => Vec::new(),
    };
    (pairs.into_iter())
        .map(|(name, value)| Var {
            line: lines.key(at, &name),
            name,
            value: defaults(&value),
            file: file.to_owned(),
        })
        .collect()
}

/// The variables of the env file `file`: `KEY=value` lines, `export` and quotes allowed.
fn env_file(file: &str, text: &str) -> Vec<Var> {
    (text.lines().enumerate())
        .filter_map(|(at, line)| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (name, value) = line.split_once('=').filter(|_| !line.starts_with('#'))?;
            let value = value.trim();
            let unquoted = (value.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')));
            Some(Var {
                name: name.trim().to_owned(),
                value: defaults(unquoted.unwrap_or(value)),
                file: file.to_owned(),
                line: at + 1,
            })
        })
        .collect()
}

/// `value` with each `${NAME:-default}` and `${NAME-default}` replaced by its default, as
/// compose interpolates them when `NAME` is unset. Other `${...}` stay as written.
fn defaults(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(open) = rest.find("${") {
        let Some(close) = rest[open..].find('}').map(|n| open + n) else {
            break;
        };
        let inner = &rest[open + 2..close];
        let name_end =
            (inner.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))).unwrap_or(inner.len());
        let tail = &inner[name_end..];
        out.push_str(&rest[..open]);
        match tail.strip_prefix(":-").or_else(|| tail.strip_prefix('-')) {
            Some(default) => out.push_str(default),
            None => out.push_str(&rest[open..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Every compose service, merged by project (folder) and name, becomes a unit; a variable
/// naming another service of its project, by name or by a link alias, is a hop. Files come base
/// first, and an override's build replaces the base's.
pub(super) fn link(infra: &mut Infra, files: Vec<(String, Vec<Service>)>) {
    let mut merged: BTreeMap<(String, String), (String, Service)> = BTreeMap::new();
    for (file, services) in files {
        for service in services {
            let key = (dir_of(&file).to_owned(), service.name.clone());
            match merged.get_mut(&key) {
                Some((_, known)) => {
                    known.vars.extend(service.vars);
                    known.aliases.extend(service.aliases);
                    known.build = service.build.or(known.build.take());
                }
                None => {
                    merged.insert(key, (file.clone(), service));
                }
            }
        }
    }
    let mut at: HashMap<(&str, &str), usize> = HashMap::new();
    for ((dir, name), (file, service)) in &merged {
        let unit = infra.unit(UnitKind::ComposeService, name, file, service.build.clone());
        at.insert((dir.as_str(), name.as_str()), unit);
    }
    for ((dir, name), (_, service)) in &merged {
        let from = at[&(dir.as_str(), name.as_str())];
        for var in &service.vars {
            for host in hosts(&var.value, host_variable(&var.name)) {
                let aliased = service.aliases.iter().find(|(alias, _)| alias == host);
                let target = aliased.map_or(host, |(_, service)| service.as_str());
                if let Some(&to) = at.get(&(dir.as_str(), target))
                    && to != from
                {
                    let ends = (End::Unit(from), End::Unit(to));
                    infra.hop(ends, Evidence::ConfiguredPath, var, host);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{hop_names, named, var, var_of};
    use super::*;

    #[test]
    fn compose_reads_environment_maps_lists_env_files_builds_and_links() {
        let file = "ops/docker-compose.yml";
        let text = "services:
  api:
    build: ../api
    environment:
      - QUEUE_URL=amqp://${MQ_HOST:-rabbit}:5672
      - PLAIN
    links:
      - rabbit:mq
  worker:
    build:
      context: ./worker
    env_file:
      - worker.env
      - path: missing.env
    environment:
      PORT: 8080
      API: http://api:80
  rabbit:
    image: rabbitmq
";
        let services = parse(file, text).unwrap();
        let names: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["api", "worker", "rabbit"]);
        let api = &services[0];
        assert_eq!(api.build.as_deref(), Some("api"));
        let vars: Vec<_> = api.vars.iter().map(var_of).collect();
        assert_eq!(vars, [("QUEUE_URL", "amqp://rabbit:5672", file, 5)]);
        assert_eq!(api.aliases, [("mq".to_owned(), "rabbit".to_owned())]);
        let worker = &services[1];
        assert_eq!(worker.build.as_deref(), Some("ops/worker"));
        assert_eq!(worker.env_files, ["ops/worker.env", "ops/missing.env"]);
        let vars: Vec<_> = worker.vars.iter().map(var_of).collect();
        assert_eq!(
            vars,
            [
                ("PORT", "8080", file, 16),
                ("API", "http://api:80", file, 17)
            ]
        );
        assert!(parse(file, "version: '3'\n").is_none());

        let env = "# comment\nexport A=\"http://rabbit:5672\"\nB='x'\n\nC\n";
        let vars = env_file("ops/worker.env", env);
        let vars: Vec<_> = vars.iter().map(var_of).collect();
        let worker_env = "ops/worker.env";
        assert_eq!(
            vars,
            [
                ("A", "http://rabbit:5672", worker_env, 2),
                ("B", "x", worker_env, 3)
            ]
        );
    }

    #[test]
    fn defaults_replace_unset_interpolations() {
        assert_eq!(
            defaults("http://${HOST:-api}:${PORT-80}/x"),
            "http://api:80/x"
        );
        assert_eq!(defaults("${HOST}:${A:-}"), "${HOST}:");
        assert_eq!(defaults("${open"), "${open");
    }

    fn service(name: &str, vars: Vec<Var>, build: Option<&str>) -> Service {
        Service {
            name: name.to_owned(),
            build: build.map(str::to_owned),
            vars,
            env_files: Vec::new(),
            aliases: Vec::new(),
        }
    }

    #[test]
    fn hops_name_services_of_the_same_project_by_name_or_alias() {
        let mut api = service(
            "api",
            vec![
                var("QUEUE", "amqp://mq:5672", "a/compose.yml", 3),
                var("SELF", "http://api:80", "a/compose.yml", 4),
                var("OTHER", "http://worker:80", "a/compose.yml", 5),
            ],
            Some("api"),
        );
        api.aliases = vec![("mq".to_owned(), "rabbit".to_owned())];
        let host = var("DB_HOST", "rabbit", "a/compose.override.yml", 2);
        let mut infra = Infra::default();
        let files = vec![
            (
                "a/compose.yml".to_owned(),
                vec![api, service("rabbit", Vec::new(), None)],
            ),
            (
                "a/compose.override.yml".to_owned(),
                vec![service("api", vec![host], Some("api2"))],
            ),
            (
                "b/compose.yml".to_owned(),
                vec![service("worker", Vec::new(), None)],
            ),
        ];
        link(&mut infra, files);
        // The worker belongs to another project, so `worker` names nothing for api; the
        // override's build replaces the base's.
        let units: Vec<(&str, &str, Option<&str>)> = (infra.units.iter())
            .map(|u| (u.name.as_str(), u.file.as_str(), u.build.as_deref()))
            .collect();
        assert_eq!(
            units,
            [
                ("api", "a/compose.yml", Some("api2")),
                ("rabbit", "a/compose.yml", None),
                ("worker", "b/compose.yml", None)
            ]
        );
        let base = "a/compose.yml";
        let over = "a/compose.override.yml";
        assert_eq!(
            hop_names(&infra),
            [
                named("api", "rabbit", base, 3, "QUEUE", "mq"),
                named("api", "rabbit", over, 2, "DB_HOST", "rabbit"),
            ]
        );
    }
}
