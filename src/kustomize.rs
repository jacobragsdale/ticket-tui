//! What one environment declares, read from the deployment repository's
//! kustomize overlays.
//!
//! The overlay is rendered, never interpreted: `kubectl kustomize <overlay>`
//! applies every patch and hash-suffixes every generated name exactly as the
//! Deployment refers to it, so a match on rendered names is a plain string
//! match and nothing here has to understand kustomize.
//!
//! Names and presence, never values. A rendered `secretGenerator` carries
//! base64 in the file; only its keys are read into the model, on the same rule
//! `Secret` keeps in `src/arm.rs`. Nothing here applies anything, reaches a
//! cluster, or reaches a vault: this is the repository's own account of itself,
//! which is what makes it answerable offline and before a merge.
//!
//! `render` shells out; `parse` and `check` are pure over one `EnvManifest`,
//! which is what the tests drive and what the diff and the board are built on.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::aks::kubectl_error;
use crate::config::{Config, Environment};
use crate::local::{self, RepoKey};

/// What renders an overlay when `config.toml` does not say. `kubectl` embeds
/// kustomize, so a machine with `kubectl` needs nothing else installed; a
/// repository that wants a newer kustomize writes `render = "kustomize build"`.
pub const DEFAULT_RENDER: &str = "kubectl kustomize";

/// The kinds a workload is read out of. Anything else in the render is skipped
/// rather than refused: an overlay holds Services, policies and CRDs this has
/// no opinion about.
const WORKLOAD_KINDS: [&str; 5] = ["Deployment", "StatefulSet", "DaemonSet", "Job", "CronJob"];

/// One environment as its overlays declare it, names only.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct EnvManifest {
    pub environment: String,
    /// The overlays that were rendered into this, relative to the clone.
    pub overlays: Vec<String>,
    pub workloads: Vec<Workload>,
    pub config_maps: Vec<KeyedObject>,
    /// The Secrets the render itself holds. Their values are not read.
    pub secrets: Vec<KeyedObject>,
    pub providers: Vec<Provider>,
}

/// One Deployment, StatefulSet, DaemonSet, Job or CronJob, and what it asks
/// for.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Workload {
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub containers: Vec<Container>,
    /// Volume references, which belong to the pod rather than to a container.
    pub volumes: Vec<Reference>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Container {
    pub name: String,
    /// Whether it is an init container, which runs before the rest.
    pub init: bool,
    pub image: String,
    pub references: Vec<Reference>,
}

/// A ConfigMap or a Secret, by the keys it holds.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct KeyedObject {
    pub namespace: String,
    pub name: String,
    pub keys: Vec<String>,
}

impl KeyedObject {
    /// Whether this is the object a reference from `namespace` names. A
    /// resource an overlay left un-namespaced is in whichever namespace the
    /// render puts the workload, so a blank on either side matches.
    #[must_use]
    pub fn is(&self, namespace: &str, name: &str) -> bool {
        self.name == name && same_namespace(&self.namespace, namespace)
    }
}

/// A `SecretProviderClass` or an `ExternalSecret`. Both come down to the same
/// two facts: which vault objects are asked for, and which Kubernetes Secret
/// keys they become.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Provider {
    /// `SecretProviderClass` or `ExternalSecret`.
    pub kind: String,
    pub namespace: String,
    pub name: String,
    /// The vault it pulls from: `keyvaultName`, or the name in the store's
    /// `vaultUrl`.
    pub vault: Option<String>,
    /// What it asks the vault for, by the vault's own names.
    pub objects: Vec<VaultObject>,
    /// The Secrets it produces, and the keys it puts in them.
    pub produces: Vec<KeyedObject>,
    /// Whether it pulls a vault object whole (`dataFrom`), which makes the
    /// keys it produces unknowable from the repository — so a reference into
    /// one of its Secrets is never called missing.
    pub whole: bool,
}

/// One thing a provider asks a vault for.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct VaultObject {
    /// The vault's own name for it.
    pub name: String,
    /// `secret`, `key` or `cert`; an `ExternalSecret` does not say.
    pub kind: Option<String>,
    /// What it is called on the way in, when it is renamed.
    pub alias: Option<String>,
}

/// Where in a workload a reference is written.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "at", rename_all = "camelCase")]
pub enum Source {
    /// `env[].valueFrom`, by the variable it fills.
    Env { var: String },
    /// `envFrom[]`, which asks for the whole object.
    EnvFrom,
    /// A volume, by its name.
    Volume { volume: String },
}

impl std::fmt::Display for Source {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Env { var } => write!(formatter, "env {var}"),
            Self::EnvFrom => formatter.write_str("envFrom"),
            Self::Volume { volume } => write!(formatter, "volume {volume}"),
        }
    }
}

/// What kind of object a reference names.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ObjectKind {
    ConfigMap,
    Secret,
    SecretProviderClass,
}

impl std::fmt::Display for ObjectKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ConfigMap => "configmap",
            Self::Secret => "secret",
            Self::SecretProviderClass => "secretproviderclass",
        })
    }
}

/// One thing a workload asks its own environment for.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Reference {
    pub source: Source,
    pub object: ObjectKind,
    pub name: String,
    /// The key inside it, where the reference names one. `envFrom` and a
    /// volume with no `items` ask for the whole object instead.
    pub key: Option<String>,
    /// `optional: true` says the workload starts without it, so it is never a
    /// finding.
    pub optional: bool,
}

impl std::fmt::Display for Reference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} \u{2190} {} {}",
            self.source, self.object, self.name
        )?;
        if let Some(key) = &self.key {
            write!(formatter, " key {key}")?;
        }
        if self.optional {
            formatter.write_str(" (optional)")?;
        }
        Ok(())
    }
}

/// What a reference points at that the overlay does not define.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Missing {
    /// The object itself is nowhere in the overlay.
    Object,
    /// The object is there without the key.
    Key,
}

/// One reference the overlay does not answer. Everything about it comes from
/// the repository, so it is knowable offline and before the merge.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Finding {
    pub environment: String,
    pub namespace: String,
    pub workload: String,
    /// The workload's kind, so the line says what to go and look at.
    pub kind: String,
    /// The container it is written on; a volume belongs to the pod and has
    /// none.
    pub container: Option<String>,
    pub reference: Reference,
    pub missing: Missing,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let place = if self.namespace.is_empty() {
            self.workload.clone()
        } else {
            format!("{}/{}", self.namespace, self.workload)
        };
        write!(
            formatter,
            "{} {place} {} {} \u{2190} {} {}",
            self.environment,
            self.kind,
            self.reference.source,
            self.reference.object,
            self.name(),
        )
    }
}

impl Finding {
    /// The object and what is missing from it: `orders-config key
    /// RATE_LIMIT_PER_MIN missing`, or `orders-config missing`.
    fn name(&self) -> String {
        match (self.missing, self.reference.key.as_deref()) {
            (Missing::Key, Some(key)) => format!("{} key {key} missing", self.reference.name),
            _ => format!("{} missing", self.reference.name),
        }
    }
}

impl EnvManifest {
    /// An environment with nothing in it yet.
    #[must_use]
    pub fn named(environment: &str) -> Self {
        Self {
            environment: environment.to_owned(),
            ..Self::default()
        }
    }

    /// Folds another overlay of the same environment in. Two overlays that
    /// render the same base resource say the same thing about it, so an exact
    /// duplicate is dropped rather than counted twice.
    pub fn absorb(&mut self, other: Self) {
        self.overlays.extend(other.overlays);
        self.workloads.extend(other.workloads);
        self.config_maps.extend(other.config_maps);
        self.secrets.extend(other.secrets);
        self.providers.extend(other.providers);
    }

    /// Sorted and deduplicated, so two runs print the same thing in the same
    /// order and a diff of them is readable.
    pub fn tidy(&mut self) {
        // By where it lives rather than by what it is, so a service's parts
        // read together and a finding is found where the line put it.
        self.workloads.sort_by(|left, right| {
            (&left.namespace, &left.name, &left.kind).cmp(&(
                &right.namespace,
                &right.name,
                &right.kind,
            ))
        });
        self.workloads.dedup();
        self.config_maps.sort();
        self.config_maps.dedup();
        self.secrets.sort();
        self.secrets.dedup();
        self.providers.sort();
        self.providers.dedup();
    }

    /// Every key the overlay gives one Secret: what the render holds, plus
    /// what any provider says it will put there. The flag is whether a
    /// provider pulls it whole, which makes the key list incomplete by nature.
    fn secret_keys(&self, namespace: &str, name: &str) -> Option<(Vec<&str>, bool)> {
        let mut found = false;
        let mut keys: Vec<&str> = Vec::new();
        let mut whole = false;
        for object in &self.secrets {
            if object.is(namespace, name) {
                found = true;
                keys.extend(object.keys.iter().map(String::as_str));
            }
        }
        for provider in &self.providers {
            for produced in &provider.produces {
                if produced.is(namespace, name) {
                    found = true;
                    whole |= provider.whole;
                    keys.extend(produced.keys.iter().map(String::as_str));
                }
            }
        }
        found.then_some((keys, whole))
    }

    /// What one reference from `namespace` asks for that is not here.
    fn missing(&self, namespace: &str, reference: &Reference) -> Option<Missing> {
        if reference.optional {
            return None;
        }
        match reference.object {
            ObjectKind::ConfigMap => {
                let mut keys: Vec<&str> = Vec::new();
                let mut found = false;
                for object in &self.config_maps {
                    if object.is(namespace, &reference.name) {
                        found = true;
                        keys.extend(object.keys.iter().map(String::as_str));
                    }
                }
                if !found {
                    return Some(Missing::Object);
                }
                let key = reference.key.as_deref()?;
                (!keys.contains(&key)).then_some(Missing::Key)
            }
            ObjectKind::Secret => {
                let Some((keys, whole)) = self.secret_keys(namespace, &reference.name) else {
                    return Some(Missing::Object);
                };
                let key = reference.key.as_deref()?;
                (!whole && !keys.contains(&key)).then_some(Missing::Key)
            }
            ObjectKind::SecretProviderClass => {
                let held = self.providers.iter().any(|provider| {
                    provider.kind == "SecretProviderClass"
                        && provider.name == reference.name
                        && same_namespace(&provider.namespace, namespace)
                });
                (!held).then_some(Missing::Object)
            }
        }
    }
}

/// Every ConfigMap and Secret reference one environment makes that its own
/// overlay does not answer. Pure over the manifest: no cluster, no vault, no
/// network. Sorted by environment, namespace and workload, so a diff of two
/// runs is readable.
#[must_use]
pub fn check(manifest: &EnvManifest) -> Vec<Finding> {
    let mut findings = Vec::new();
    for workload in &manifest.workloads {
        let mut record = |container: Option<&Container>, reference: &Reference| {
            if let Some(missing) = manifest.missing(&workload.namespace, reference) {
                findings.push(Finding {
                    environment: manifest.environment.clone(),
                    namespace: workload.namespace.clone(),
                    workload: workload.name.clone(),
                    kind: workload.kind.clone(),
                    container: container.map(|held| held.name.clone()),
                    reference: reference.clone(),
                    missing,
                });
            }
        };
        for container in &workload.containers {
            for reference in &container.references {
                record(Some(container), reference);
            }
        }
        for reference in &workload.volumes {
            record(None, reference);
        }
    }
    findings.sort_by(|left, right| {
        (&left.environment, &left.namespace, &left.workload).cmp(&(
            &right.environment,
            &right.namespace,
            &right.workload,
        ))
    });
    findings
}

/// One rendered overlay, read into the model. Pure, and infallible: a document
/// of a kind this has no opinion about — a Service, a policy, somebody's CRD —
/// is skipped rather than refused, because an overlay is full of them.
#[must_use]
pub fn parse(environment: &str, yaml: &str) -> EnvManifest {
    let documents = documents(yaml);
    // The stores first: an ExternalSecret names one rather than naming a vault.
    let stores: Vec<(String, String, String)> = documents
        .iter()
        .filter(|document| {
            matches!(
                text(document, &["kind"]),
                "SecretStore" | "ClusterSecretStore"
            )
        })
        .filter_map(|document| {
            let vault = vault_name(text(document, &["spec", "provider", "azurekv", "vaultUrl"]))?;
            Some((
                text(document, &["metadata", "namespace"]).to_owned(),
                text(document, &["metadata", "name"]).to_owned(),
                vault,
            ))
        })
        .collect();

    let mut manifest = EnvManifest::named(environment);
    for document in &documents {
        let kind = text(document, &["kind"]);
        let namespace = text(document, &["metadata", "namespace"]).to_owned();
        let name = text(document, &["metadata", "name"]).to_owned();
        if name.is_empty() {
            continue;
        }
        if WORKLOAD_KINDS.contains(&kind) {
            manifest
                .workloads
                .push(workload(kind, namespace, name, document));
        } else if kind == "ConfigMap" {
            manifest.config_maps.push(KeyedObject {
                namespace,
                name,
                keys: field_keys(document, &["data", "binaryData"]),
            });
        } else if kind == "Secret" {
            manifest.secrets.push(KeyedObject {
                namespace,
                name,
                keys: field_keys(document, &["data", "stringData"]),
            });
        } else if kind == "SecretProviderClass" {
            manifest
                .providers
                .push(secret_provider_class(namespace, name, document));
        } else if kind == "ExternalSecret" {
            manifest
                .providers
                .push(external_secret(namespace, name, document, &stores));
        }
    }
    manifest.tidy();
    manifest
}

/// One workload, with every reference its containers and volumes make.
fn workload(kind: &str, namespace: String, name: String, document: &Value) -> Workload {
    let spec = pod_spec(document);
    let mut containers = Vec::new();
    for (field, init) in [("containers", false), ("initContainers", true)] {
        for held in array(spec, &[field]) {
            containers.push(Container {
                name: text(held, &["name"]).to_owned(),
                init,
                image: text(held, &["image"]).to_owned(),
                references: container_references(held),
            });
        }
    }
    Workload {
        kind: kind.to_owned(),
        namespace,
        name,
        containers,
        volumes: volume_references(spec),
    }
}

/// Where the pod is written: under `spec.template` on everything but a
/// CronJob, which puts a Job template in between.
fn pod_spec(document: &Value) -> &Value {
    let spec = &document["spec"];
    if spec["jobTemplate"].is_object() {
        &spec["jobTemplate"]["spec"]["template"]["spec"]
    } else {
        &spec["template"]["spec"]
    }
}

/// `env[].valueFrom` and `envFrom[]`, in the order they are written.
fn container_references(container: &Value) -> Vec<Reference> {
    let mut references = Vec::new();
    for entry in array(container, &["env"]) {
        let source = Source::Env {
            var: text(entry, &["name"]).to_owned(),
        };
        for (field, object) in [
            ("configMapKeyRef", ObjectKind::ConfigMap),
            ("secretKeyRef", ObjectKind::Secret),
        ] {
            let target = &entry["valueFrom"][field];
            if let Some(name) = target["name"].as_str() {
                references.push(Reference {
                    source: source.clone(),
                    object,
                    name: name.to_owned(),
                    key: target["key"].as_str().map(str::to_owned),
                    optional: flag(target, &["optional"]),
                });
            }
        }
    }
    for entry in array(container, &["envFrom"]) {
        for (field, object) in [
            ("configMapRef", ObjectKind::ConfigMap),
            ("secretRef", ObjectKind::Secret),
        ] {
            if let Some(name) = entry[field]["name"].as_str() {
                references.push(Reference {
                    source: Source::EnvFrom,
                    object,
                    name: name.to_owned(),
                    key: None,
                    optional: flag(&entry[field], &["optional"]),
                });
            }
        }
    }
    references
}

/// What the pod's volumes ask for: a ConfigMap or a Secret — whole, or by the
/// keys `items` names — and the `SecretProviderClass` a CSI volume mounts.
fn volume_references(spec: &Value) -> Vec<Reference> {
    let mut references = Vec::new();
    for entry in array(spec, &["volumes"]) {
        let source = Source::Volume {
            volume: text(entry, &["name"]).to_owned(),
        };
        for (field, name_field, object) in [
            ("configMap", "name", ObjectKind::ConfigMap),
            ("secret", "secretName", ObjectKind::Secret),
        ] {
            let target = &entry[field];
            let Some(name) = target[name_field].as_str() else {
                continue;
            };
            let optional = flag(target, &["optional"]);
            let items: Vec<&str> = array(target, &["items"])
                .iter()
                .filter_map(|item| item["key"].as_str())
                .collect();
            if items.is_empty() {
                references.push(Reference {
                    source: source.clone(),
                    object,
                    name: name.to_owned(),
                    key: None,
                    optional,
                });
            }
            for key in items {
                references.push(Reference {
                    source: source.clone(),
                    object,
                    name: name.to_owned(),
                    key: Some(key.to_owned()),
                    optional,
                });
            }
        }
        if let Some(class) = entry["csi"]["volumeAttributes"]["secretProviderClass"].as_str() {
            references.push(Reference {
                source: source.clone(),
                object: ObjectKind::SecretProviderClass,
                name: class.to_owned(),
                key: None,
                optional: false,
            });
        }
    }
    references
}

/// A `SecretProviderClass`: the vault it names, the objects it pulls — written
/// as YAML inside the YAML — and the Secrets `secretObjects` makes of them.
fn secret_provider_class(namespace: String, name: String, document: &Value) -> Provider {
    let parameters = &document["spec"]["parameters"];
    let produces = array(document, &["spec", "secretObjects"])
        .iter()
        .filter_map(|object| {
            let secret = object["secretName"].as_str()?;
            Some(KeyedObject {
                namespace: namespace.clone(),
                name: secret.to_owned(),
                keys: array(object, &["data"])
                    .iter()
                    .filter_map(|entry| entry["key"].as_str().map(str::to_owned))
                    .collect(),
            })
        })
        .collect();
    Provider {
        kind: "SecretProviderClass".to_owned(),
        namespace,
        name,
        vault: Some(text(parameters, &["keyvaultName"]).to_owned()).filter(|held| !held.is_empty()),
        objects: vault_objects(text(parameters, &["objects"])),
        produces,
        whole: false,
    }
}

/// An `ExternalSecret`: the vault its store points at, the remote keys it
/// reads, and the one Secret it writes.
fn external_secret(
    namespace: String,
    name: String,
    document: &Value,
    stores: &[(String, String, String)],
) -> Provider {
    let store = text(document, &["spec", "secretStoreRef", "name"]);
    let vault = stores
        .iter()
        .find(|(held, called, _)| called == store && same_namespace(held, &namespace))
        .map(|(_, _, vault)| vault.clone());
    let data = array(document, &["spec", "data"]);
    let mut objects: Vec<VaultObject> = data
        .iter()
        .filter_map(|entry| {
            Some(VaultObject {
                name: entry["remoteRef"]["key"].as_str()?.to_owned(),
                kind: None,
                alias: entry["secretKey"].as_str().map(str::to_owned),
            })
        })
        .collect();
    // `dataFrom` pulls a vault object whole, so it names an object without
    // naming any of the keys it will turn into.
    let whole_from = array(document, &["spec", "dataFrom"]);
    objects.extend(whole_from.iter().filter_map(|entry| {
        Some(VaultObject {
            name: entry["extract"]["key"].as_str()?.to_owned(),
            kind: None,
            alias: None,
        })
    }));
    let target = document["spec"]["target"]["name"]
        .as_str()
        .unwrap_or(&name)
        .to_owned();
    let produces = vec![KeyedObject {
        namespace: namespace.clone(),
        name: target,
        keys: data
            .iter()
            .filter_map(|entry| entry["secretKey"].as_str().map(str::to_owned))
            .collect(),
    }];
    Provider {
        kind: "ExternalSecret".to_owned(),
        namespace,
        name,
        vault,
        objects,
        produces,
        whole: !whole_from.is_empty(),
    }
}

/// A `SecretProviderClass`'s `objects`: one YAML document held in a string,
/// whose `array` entries are themselves YAML documents in strings — which is
/// how the CSI driver reads them — or plain mappings, which is how they are
/// often written.
fn vault_objects(raw: &str) -> Vec<VaultObject> {
    let Ok(parsed) = serde_yaml_ng::from_str::<Value>(raw) else {
        return Vec::new();
    };
    array(&parsed, &["array"])
        .iter()
        .filter_map(|entry| {
            let nested;
            let entry = match entry.as_str() {
                Some(written) => {
                    nested = serde_yaml_ng::from_str::<Value>(written).ok()?;
                    &nested
                }
                None => entry,
            };
            Some(VaultObject {
                name: entry["objectName"].as_str()?.to_owned(),
                kind: entry["objectType"].as_str().map(str::to_owned),
                alias: entry["objectAlias"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

/// `kv-prod` out of `https://kv-prod.vault.azure.net/`, which is how a vault a
/// store names joins the vaults the Key Vault tab lists.
fn vault_name(url: &str) -> Option<String> {
    let (_, tail) = url.split_once("://")?;
    let label = tail.split('/').next()?.split('.').next()?;
    (!label.is_empty()).then(|| label.to_owned())
}

/// A resource an overlay left un-namespaced lands in whichever namespace the
/// render puts the workload, so a blank on either side matches.
fn same_namespace(left: &str, right: &str) -> bool {
    left.is_empty() || right.is_empty() || left == right
}

/// The documents of one `---`-separated stream, as JSON values. One that will
/// not read as a mapping is dropped: the stream ends with a document break and
/// a comment often stands alone.
fn documents(yaml: &str) -> Vec<Value> {
    serde_yaml_ng::Deserializer::from_str(yaml)
        .filter_map(|document| Value::deserialize(document).ok())
        .filter(Value::is_object)
        .collect()
}

/// The value at a path, as a string; `""` for anything that is not there or is
/// not a string.
fn text<'a>(value: &'a Value, path: &[&str]) -> &'a str {
    at(value, path).as_str().unwrap_or_default()
}

fn flag(value: &Value, path: &[&str]) -> bool {
    at(value, path).as_bool().unwrap_or(false)
}

fn array<'a>(value: &'a Value, path: &[&str]) -> &'a [Value] {
    at(value, path).as_array().map_or(&[], Vec::as_slice)
}

fn at<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(value, |held, step| &held[step])
}

/// The keys of every one of `fields` that is a mapping, sorted. The values are
/// not read: a rendered Secret carries base64 in the file, and nothing here
/// takes it out.
fn field_keys(document: &Value, fields: &[&str]) -> Vec<String> {
    let mut keys: Vec<String> = fields
        .iter()
        .filter_map(|field| document[field].as_object())
        .flat_map(|map| map.keys().cloned())
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// The clone of the deployment repository, found the way the Repos tab finds
/// any other: the workspace scan, matching by remote and then by name. The
/// error says where it looked, because that is the thing to fix.
pub fn deployment_clone(config: &Config, workspace: Option<&Path>) -> Result<PathBuf> {
    let deployment = config
        .deployment
        .as_ref()
        .context("no [deployment] in config.toml; add one naming the deployment repository")?;
    let workspace = workspace.context(
        "no workspace to look in; set --workspace, TICKET_TUI_WORKSPACE or devops.workspace",
    )?;
    let repo = deployment.repo.clone();
    local::scan(
        workspace,
        &[RepoKey {
            id: repo.clone(),
            remote: None,
            name: repo.clone(),
        }],
    )
    .into_iter()
    .next()
    .map(|(_, local)| local.path)
    .with_context(|| format!("no clone of {repo} in {}", workspace.display()))
}

/// The overlay directories one pattern names, relative to the clone. `*`
/// matches within one path segment, so `services/*/overlays/prod` is every
/// service's; a pattern with no `*` is the one path it spells.
#[must_use]
pub fn expand(clone: &Path, pattern: &str) -> Vec<String> {
    let mut matched = vec![String::new()];
    for step in pattern.split('/').filter(|step| !step.is_empty()) {
        if !step.contains('*') {
            for held in &mut matched {
                if !held.is_empty() {
                    held.push('/');
                }
                held.push_str(step);
            }
            continue;
        }
        let mut next = Vec::new();
        for held in &matched {
            let Ok(entries) = std::fs::read_dir(clone.join(held)) else {
                continue;
            };
            let mut here: Vec<String> = entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| segment_matches(step, name))
                .map(|name| {
                    if held.is_empty() {
                        name
                    } else {
                        format!("{held}/{name}")
                    }
                })
                .collect();
            here.sort();
            next.extend(here);
        }
        matched = next;
    }
    matched
        .into_iter()
        .filter(|held| !held.is_empty() && clone.join(held).is_dir())
        .collect()
}

/// One path segment against one pattern, `*` matching any run of characters
/// within the segment. A hand-rolled matcher rather than a dependency: this is
/// the whole of the glob the file needs.
fn segment_matches(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let last = parts[parts.len() - 1];
    let Some(mut rest) = name.strip_prefix(parts[0]) else {
        return false;
    };
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// One overlay, rendered. `kubectl kustomize <overlay>` applies every patch and
/// hash-suffixes every generated name, so what comes back is what the cluster
/// would be given; a render that fails comes back as the one line of the
/// renderer's complaint that says what to fix, the way every `kubectl` call
/// here does.
pub fn render(clone: &Path, overlay: &str, command: &str) -> Result<String> {
    let mut words = command.split_whitespace();
    let program = words.next().unwrap_or("kubectl");
    let output = Command::new(program)
        .args(words)
        .arg(clone.join(overlay))
        .stdin(Stdio::null())
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("{program} is not installed or not on PATH")
        }
        Err(error) => return Err(error).with_context(|| format!("{program} could not be run")),
    };
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    bail!(
        "{overlay}: {}",
        kubectl_error(&String::from_utf8_lossy(&output.stderr))
    )
}

/// Every overlay one environment names, rendered and unioned into one model.
pub fn manifest(clone: &Path, environment: &Environment, command: &str) -> Result<EnvManifest> {
    let overlays: Vec<String> = environment
        .overlays
        .iter()
        .flat_map(|pattern| expand(clone, pattern))
        .collect();
    if overlays.is_empty() {
        bail!(
            "no overlay in {} matches {}",
            clone.display(),
            environment.overlays.join(", ")
        );
    }
    let mut manifest = EnvManifest::named(&environment.name);
    for overlay in overlays {
        let mut rendered = parse(&environment.name, &render(clone, &overlay, command)?);
        rendered.overlays.push(overlay);
        manifest.absorb(rendered);
    }
    manifest.tidy();
    Ok(manifest)
}

#[cfg(test)]
mod tests;
