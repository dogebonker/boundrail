//! Kubernetes manifests: workloads with their container variables, Services with their
//! selectors, and ConfigMaps.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use super::{End, Infra, Lines, UnitKind, Var, host_variable, hosts, scalar};
use crate::arch::types::Evidence;
use crate::modules::dir_of;

/// Workload kinds whose pod template is at `spec.template`.
const WORKLOADS: [&str; 4] = ["Deployment", "StatefulSet", "DaemonSet", "Job"];
/// The workload kind whose pod template is at `spec.jobTemplate.spec.template`.
const CRON_JOB: &str = "CronJob";
const SERVICE: &str = "Service";
const CONFIG_MAP: &str = "ConfigMap";
/// The pod spec keys holding containers.
const CONTAINERS: [&str; 2] = ["containers", "initContainers"];
const DEFAULT_NAMESPACE: &str = "default";
/// The suffixes a Service's `name.namespace` DNS name takes.
const DNS_SUFFIXES: [&str; 2] = [".svc.cluster.local", ".svc"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectKind {
    Workload,
    Service,
    ConfigMap,
}

/// One Kubernetes object the collector reads.
#[derive(Debug)]
pub(super) struct Object {
    file: String,
    kind: ObjectKind,
    name: String,
    /// `None` when the manifest names none.
    namespace: Option<String>,
    /// A workload's pod labels, or a Service's selector.
    labels: BTreeMap<String, String>,
    /// A workload's container variables.
    env: Vec<Env>,
    /// A ConfigMap's data.
    data: Vec<Var>,
}

#[derive(Debug)]
enum Env {
    Value(Var),
    /// `valueFrom.configMapKeyRef`: the variable, the ConfigMap and its key.
    ConfigMap(String, String, String),
}

/// The workloads, Services and ConfigMaps of the multi-document manifest `file`.
pub(super) fn parse(file: &str, text: &str) -> Vec<Object> {
    let lines = Lines::new(text);
    let mut starts = vec![0];
    starts.extend(
        (lines.0.iter().enumerate())
            .filter(|(_, l)| l.starts_with("---"))
            .map(|(at, _)| at + 1),
    );
    let mut objects = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map_or(lines.0.len(), |next| next - 1);
        let doc = lines.0[start..end.max(start)].join("\n");
        if let Ok(value) = serde_yaml_ng::from_str::<Value>(&doc) {
            objects.extend(object(file, &lines, start + 1, &value));
        }
    }
    objects
}

/// The object a manifest document starting on line `at` declares, if the collector reads it.
fn object(file: &str, lines: &Lines<'_>, at: usize, doc: &Value) -> Option<Object> {
    let kind = doc.get("kind")?.as_str()?;
    let meta = doc.get("metadata")?;
    let spec = doc.get("spec");
    let mut object = Object {
        file: file.to_owned(),
        kind: ObjectKind::Workload,
        name: meta.get("name")?.as_str()?.to_owned(),
        namespace: (meta.get("namespace").and_then(Value::as_str)).map(str::to_owned),
        labels: BTreeMap::new(),
        env: Vec::new(),
        data: Vec::new(),
    };
    let template = if kind == CRON_JOB {
        spec?.get("jobTemplate")?.get("spec")
    } else {
        spec
    };
    match kind {
        SERVICE => {
            object.kind = ObjectKind::Service;
            object.labels = labels(spec.and_then(|s| s.get("selector")));
        }
        CONFIG_MAP => {
            object.kind = ObjectKind::ConfigMap;
            object.data = config_data(file, lines, at, doc.get("data"));
        }
        _ if kind == CRON_JOB || WORKLOADS.contains(&kind) => {
            let template = template?.get("template")?;
            object.labels = labels(template.get("metadata").and_then(|m| m.get("labels")));
            object.env = container_env(file, lines, at, template.get("spec"));
        }
        _ => return None,
    }
    Some(object)
}

fn labels(map: Option<&Value>) -> BTreeMap<String, String> {
    (map.and_then(Value::as_object).into_iter().flatten())
        .filter_map(|(k, v)| Some((k.clone(), scalar(v)?)))
        .collect()
}

/// A ConfigMap's data as variables, each on the line that sets it.
fn config_data(file: &str, lines: &Lines<'_>, at: usize, data: Option<&Value>) -> Vec<Var> {
    (data.and_then(Value::as_object).into_iter().flatten())
        .filter_map(|(key, value)| {
            Some(Var {
                name: key.clone(),
                value: scalar(value)?,
                file: file.to_owned(),
                line: lines.key(at, key),
            })
        })
        .collect()
}

/// Whether `line` is an env entry's `name: <name>`.
fn names(line: &str, name: &str) -> bool {
    let line = line.trim_start();
    let line = line.strip_prefix('-').map_or(line, str::trim_start);
    (line.strip_prefix("name:")).is_some_and(|v| v.trim().trim_matches(['"', '\'']) == name)
}

/// The variables of every container and init container of a pod spec.
fn container_env(file: &str, lines: &Lines<'_>, at: usize, pod: Option<&Value>) -> Vec<Env> {
    let containers = (CONTAINERS.iter())
        .filter_map(|key| pod?.get(key)?.as_array())
        .flatten();
    let entries = containers
        .filter_map(|c| c.get("env")?.as_array())
        .flatten();
    (entries.filter_map(|entry| {
        let name = entry.get("name")?.as_str()?;
        if let Some(value) = entry.get("value").and_then(scalar) {
            let named = lines.find(at, |l| names(l, name)).unwrap_or(at);
            let line = lines.key(named, "value");
            let (name, file) = (name.to_owned(), file.to_owned());
            return Some(Env::Value(Var {
                name,
                value,
                file,
                line,
            }));
        }
        let reference = entry.get("valueFrom")?.get("configMapKeyRef")?;
        let map = reference.get("name")?.as_str()?.to_owned();
        let key = reference.get("key")?.as_str()?.to_owned();
        Some(Env::ConfigMap(name.to_owned(), map, key))
    }))
    .collect()
}

/// Whether `object` is in `namespace`, seen from `from`: an object that names no namespace is
/// in any namespace of its own folder, and else in the default one.
fn in_namespace(object: &Object, namespace: &str, from: &Object) -> bool {
    match &object.namespace {
        Some(own) => own == namespace,
        None => dir_of(&object.file) == dir_of(&from.file) || namespace == DEFAULT_NAMESPACE,
    }
}

/// Whether `a` and `b` share a namespace, either naming none matching as [`in_namespace`].
fn same_namespace(a: &Object, b: &Object) -> bool {
    match (&a.namespace, &b.namespace) {
        (_, Some(namespace)) => in_namespace(a, namespace, b),
        (Some(namespace), None) => in_namespace(b, namespace, a),
        (None, None) => true,
    }
}

/// The Service name and namespace a host names: `name`, or `name.namespace` with an optional
/// `.svc` or `.svc.cluster.local`.
fn dns(host: &str) -> Option<(&str, Option<&str>)> {
    let suffixed = DNS_SUFFIXES.iter().find_map(|s| host.strip_suffix(s));
    match suffixed.unwrap_or(host).split_once('.') {
        Some((name, namespace)) if !namespace.contains('.') => Some((name, Some(namespace))),
        None if suffixed.is_none() => Some((host, None)),
        _ => None,
    }
}

/// The objects of `objects` of one kind, by name.
fn by_name(objects: &[Object], kind: ObjectKind) -> HashMap<&str, Vec<&Object>> {
    let mut found: HashMap<&str, Vec<&Object>> = HashMap::new();
    for object in objects.iter().filter(|o| o.kind == kind) {
        found.entry(object.name.as_str()).or_default().push(object);
    }
    found
}

/// A workload variable with its value, resolving a ConfigMap key against the ConfigMaps of
/// the workload's namespace; the variable is then where the ConfigMap sets the key.
fn resolve(env: &Env, workload: &Object, maps: &HashMap<&str, Vec<&Object>>) -> Option<Var> {
    match env {
        Env::Value(var) => Some(var.clone()),
        Env::ConfigMap(variable, map, key) => {
            let found = (maps.get(map.as_str())?.iter()).find(|m| same_namespace(m, workload))?;
            let data = found.data.iter().find(|d| d.name == *key)?;
            Some(Var {
                name: variable.clone(),
                ..data.clone()
            })
        }
    }
}

/// The Services with a selector that `host` names, seen from `workload`.
fn services<'o>(
    host: &str,
    workload: &Object,
    services: &HashMap<&str, Vec<&'o Object>>,
) -> Vec<&'o Object> {
    let Some((name, namespace)) = dns(host) else {
        return Vec::new();
    };
    (services.get(name).into_iter().flatten())
        .filter(|s| !s.labels.is_empty())
        .filter(|s| match namespace {
            Some(namespace) => in_namespace(s, namespace, workload),
            None => same_namespace(s, workload),
        })
        .copied()
        .collect()
}

/// Every workload becomes a unit; a variable naming a Service's DNS name is a hop to each
/// workload of the Service's namespace it selects.
pub(super) fn link(infra: &mut Infra, objects: &[Object]) {
    let workloads: Vec<(usize, &Object)> = (objects.iter())
        .filter(|o| o.kind == ObjectKind::Workload)
        .map(|o| (infra.unit(UnitKind::K8sWorkload, &o.name, &o.file, None), o))
        .collect();
    let maps = by_name(objects, ObjectKind::ConfigMap);
    let named = by_name(objects, ObjectKind::Service);
    for &(from, workload) in &workloads {
        let vars = (workload.env.iter()).filter_map(|e| resolve(e, workload, &maps));
        for var in vars {
            for host in hosts(&var.value, host_variable(&var.name)) {
                for service in services(host, workload, &named) {
                    let selected = (workloads.iter()).filter(|(to, target)| {
                        *to != from
                            && same_namespace(service, target)
                            && (service.labels.iter()).all(|(k, v)| target.labels.get(k) == Some(v))
                    });
                    for &(to, _) in selected {
                        let ends = (End::Unit(from), End::Unit(to));
                        infra.hop(ends, Evidence::ConfiguredPath, &var, host);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{hop_names, named};
    use super::*;

    const MANIFEST: &str = "apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  namespace: shop
spec:
  template:
    metadata:
      labels:
        app: web
    spec:
      containers:
        - name: web
          env:
            - name: API_URL
              valueFrom:
                configMapKeyRef:
                  name: web-config
                  key: api
            - name: LEDGER
              value: http://ledger.money.svc.cluster.local:80
            - name: SELF
              value: http://web:80
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: web-config
  namespace: shop
data:
  api: http://api:8080
---
kind: Service
metadata:
  name: api
  namespace: shop
spec:
  selector:
    app: api
---
kind: Deployment
metadata:
  name: api
  namespace: shop
spec:
  template:
    metadata:
      labels: {app: api, tier: back}
    spec:
      containers: [{name: api}]
---
kind: Service
metadata:
  name: ledger
  namespace: money
spec:
  selector: {app: ledger}
---
kind: CronJob
metadata:
  name: ledger
  namespace: money
spec:
  jobTemplate:
    spec:
      template:
        metadata:
          labels: {app: ledger}
        spec:
          containers:
            - name: ledger
              env:
                - name: API_HOST
                  value: api
";

    #[test]
    fn config_maps_and_service_dns_names_resolve_to_selected_workloads() {
        let objects = parse("k8s/app.yaml", MANIFEST);
        let kinds: Vec<(ObjectKind, &str)> = (objects.iter())
            .map(|o| (o.kind, o.name.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (ObjectKind::Workload, "web"),
                (ObjectKind::ConfigMap, "web-config"),
                (ObjectKind::Service, "api"),
                (ObjectKind::Workload, "api"),
                (ObjectKind::Service, "ledger"),
                (ObjectKind::Workload, "ledger"),
            ]
        );
        let mut infra = Infra::default();
        link(&mut infra, &objects);
        // The ledger's bare `api` is in another namespace than the Service, so it names nothing.
        let ledger = "ledger.money.svc.cluster.local";
        assert_eq!(
            hop_names(&infra),
            [
                named("web", "api", "k8s/app.yaml", 31, "API_URL", "api"),
                named("web", "ledger", "k8s/app.yaml", 21, "LEDGER", ledger),
            ]
        );
        assert_eq!(dns("api"), Some(("api", None)));
        assert_eq!(dns("api.shop"), Some(("api", Some("shop"))));
        assert_eq!(dns("api.shop.svc"), Some(("api", Some("shop"))));
        assert_eq!(dns(ledger), Some(("ledger", Some("money"))));
        assert_eq!(dns("api.example.com"), None);
        assert_eq!(dns("api.svc"), None);
    }

    #[test]
    fn objects_without_a_namespace_match_any_namespace_of_their_folder() {
        let workload = |file: &str, name: &str, namespace: Option<&str>, value: &str| {
            let env = format!(
                "kind: Deployment\nmetadata: {{name: {name}{}}}\nspec:\n  template:\n    metadata: {{labels: {{app: {name}}}}}\n    spec:\n      containers:\n        - name: c\n          env:\n            - name: TARGET\n              value: {value}\n",
                namespace
                    .map(|n| format!(", namespace: {n}"))
                    .unwrap_or_default()
            );
            parse(file, &env)
        };
        let service = "kind: Service\nmetadata: {name: api}\nspec: {selector: {app: api}}\n";
        let mut objects = parse("k8s/a/service.yaml", service);
        objects.extend(workload("k8s/a/api.yaml", "api", None, "none"));
        objects.extend(workload(
            "k8s/a/web.yaml",
            "web",
            Some("shop"),
            "http://api:80",
        ));
        objects.extend(workload(
            "k8s/b/far.yaml",
            "far",
            Some("shop"),
            "http://api:80",
        ));
        objects.extend(workload(
            "k8s/b/dot.yaml",
            "dot",
            Some("prod"),
            "http://api.default:80",
        ));
        let mut infra = Infra::default();
        link(&mut infra, &objects);
        let hops: Vec<(String, String)> = (hop_names(&infra).into_iter())
            .map(|h| (h.0, h.1))
            .collect();
        // `far` sits in another folder and another namespace than the default.
        let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        assert_eq!(hops, [pair("web", "api"), pair("dot", "api")]);
    }
}
