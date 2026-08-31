//! The ARM worker: one thread for both subscription tabs, and what they ask
//! it for.
//!
//! Nothing here touches SQLite. What a subscription holds is read live, the
//! way a cluster's pods are, and the next read replaces it. The thread is its
//! own so that a registry that will not answer never queues behind a pull, and
//! so that the one `az account show` a run may need happens here rather than
//! on the startup path.
//!
//! The reads are edge-triggered: the inventory on a cadence while either tab
//! is showing, and everything under it once per focus. Drilling into a
//! registry and back out again costs nothing until `Refresh` says to read it
//! all over.

use std::cell::Cell;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::app::TabId;
use crate::arm::{
    ArmClient, ArmConfig, ArmSource, Inventory, Manifest, Registry, Repository, Secret, Tag,
    VaultItem,
};
use crate::watch::Cadence;

/// How often the subscription's registries and vaults are read while one of
/// the two tabs is showing. They change on a human timescale.
pub const INVENTORY_CADENCE: Duration = Duration::from_secs(60);

/// What the tab on screen is looking at, and so what is worth one read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArmFocus {
    /// One registry's catalog, and the attributes of everything in it.
    Registry(String),
    /// Everything one vault holds: its secrets, keys and certificates, by
    /// name. Never their values.
    Vault(String),
    /// One repository's tags.
    Repository { registry: String, name: String },
    /// What one tag points at.
    Tag {
        registry: String,
        repo: String,
        digest: String,
    },
}

/// What the run tells the worker. Each is a statement about what is worth
/// reading, so the worker can be told the same thing twice without harm.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArmRequest {
    /// Which of the two ARM tabs is showing, if either. Neither, and the
    /// worker goes quiet.
    TabShowing(Option<TabId>),
    Focus(ArmFocus),
    Blur,
    /// One secret's value, because somebody pressed the key that asks for it.
    /// Read at once rather than at the next poll, answered once, and never
    /// held here: reading one is audited, so it happens when it is asked for
    /// and not a moment besides.
    Reveal {
        vault: String,
        name: String,
    },
    /// Read the inventory now, and everything the focus asks for again.
    Refresh,
    Stop,
}

/// What the worker has read. None of it is written anywhere: the screens show
/// it, and the next read replaces it.
#[derive(Clone, Debug)]
pub enum ArmEvent {
    /// The subscription the thread settled on when startup could not: `az
    /// account show` runs here, off the startup path.
    Subscription(Result<String, String>),
    Inventory(Result<Inventory, String>),
    /// One registry's catalog: names and nothing else, which is all a catalog
    /// listing carries.
    Repositories {
        registry: String,
        repositories: Result<Vec<Repository>, String>,
    },
    /// One repository's counts and stamp, one event each as they land.
    Repository {
        registry: String,
        repository: Result<Repository, String>,
    },
    Tags {
        registry: String,
        repo: String,
        tags: Result<Vec<Tag>, String>,
    },
    Manifest {
        registry: String,
        repo: String,
        digest: String,
        manifest: Result<Manifest, String>,
    },
    /// Everything one vault holds, as its listings describe it. A listing
    /// never carries a value, which is the point of listing one.
    Items {
        vault: String,
        items: Result<Vec<VaultItem>, String>,
    },
    /// One secret's value, for the screen that asked and nowhere else. The
    /// `Debug` this derives is safe only because [`Secret`] redacts itself.
    Revealed {
        vault: String,
        name: String,
        value: Result<Secret, String>,
    },
    /// Azure asked to be left alone, and for how long. Not an error.
    Throttled(Duration),
    /// A read failed, or there is no subscription to read.
    Failed(String),
    /// The thread is gone.
    Stopped,
}

/// The worker's own state, apart from the thread it usually runs on, so a test
/// can drive it with a clock of its own.
pub struct ArmWatcher {
    source: Box<dyn ArmSource>,
    events: Sender<ArmEvent>,
    inventory: Cadence,
    /// Which ARM tab is showing, if either.
    showing: Option<TabId>,
    focus: Option<ArmFocus>,
    /// The inventory as last read, which is where a focus finds the registry
    /// it names.
    held: Inventory,
    /// The focuses already answered, so a re-focus costs nothing until
    /// `Refresh`.
    read: Vec<ArmFocus>,
}

impl ArmWatcher {
    #[must_use]
    pub fn new(source: Box<dyn ArmSource>, events: Sender<ArmEvent>) -> Self {
        Self {
            source,
            events,
            inventory: Cadence::new(INVENTORY_CADENCE),
            showing: None,
            focus: None,
            held: Inventory::default(),
            read: Vec::new(),
        }
    }

    /// One request. Answers whether to keep going.
    pub fn handle(&mut self, request: ArmRequest) -> bool {
        match request {
            ArmRequest::Stop => return false,
            ArmRequest::TabShowing(tab) => self.showing = tab,
            ArmRequest::Focus(focus) => self.focus = Some(focus),
            ArmRequest::Blur => self.focus = None,
            // Not a statement about what is worth reading: a keystroke, read
            // here and now, and answered whatever the poll is doing.
            ArmRequest::Reveal { vault, name } => self.reveal(&vault, &name),
            // Due at once, and everything under it worth reading again.
            ArmRequest::Refresh => {
                self.inventory = Cadence::new(INVENTORY_CADENCE);
                self.read.clear();
            }
        }
        true
    }

    /// Reads whatever is due. Nothing while both tabs are hidden: a
    /// subscription nobody is looking at is not worth a request.
    pub fn poll(&mut self, now: Instant) {
        if self.showing.is_none() {
            return;
        }
        if self.inventory.is_due(now) {
            let read = self.source.inventory();
            match &read {
                Ok(inventory) => {
                    self.held = inventory.clone();
                    self.inventory.relax();
                }
                Err(_) => self.inventory.stretch(),
            }
            self.inventory.polled(now);
            self.send(ArmEvent::Inventory(read.map_err(|error| said(&error))));
        }
        self.read_focus();
        // One read makes several calls, and the longest wait any of them asked
        // for is the one that has to be honoured.
        if let Some(wait) = self.source.throttled_for() {
            self.inventory.hold_off(now, wait);
            self.send(ArmEvent::Throttled(wait));
        }
    }

    /// How long until the inventory is due, or `None` while neither tab is
    /// showing.
    #[must_use]
    pub fn until_due(&self, now: Instant) -> Option<Duration> {
        self.showing.map(|_| self.inventory.until_due(now))
    }

    /// Whatever the focus asks for, once per focus. A focus naming a registry
    /// or a vault the inventory has not brought back yet is left for the read
    /// that will.
    fn read_focus(&mut self) {
        let Some(focus) = self.focus.clone() else {
            return;
        };
        if self.read.contains(&focus) {
            return;
        }
        if let ArmFocus::Vault(name) = &focus {
            let Some(vault) = self
                .held
                .vaults
                .iter()
                .find(|held| held.name == *name)
                .cloned()
            else {
                return;
            };
            self.read.push(focus);
            let items = self.source.items(&vault);
            self.send(ArmEvent::Items {
                vault: vault.name,
                items: items.map_err(|error| said(&error)),
            });
            return;
        }
        // Everything else names a registry, whatever else it names besides.
        let (ArmFocus::Registry(name)
        | ArmFocus::Repository { registry: name, .. }
        | ArmFocus::Tag { registry: name, .. }) = &focus
        else {
            return;
        };
        let name = name.clone();
        let Some(registry) = self
            .held
            .registries
            .iter()
            .find(|held| held.name == name)
            .cloned()
        else {
            return;
        };
        self.read.push(focus.clone());
        match focus {
            ArmFocus::Registry(_) => self.read_catalog(&registry),
            ArmFocus::Repository { name: repo, .. } => {
                let tags = self.source.tags(&registry, &repo);
                self.send(ArmEvent::Tags {
                    registry: name,
                    repo,
                    tags: tags.map_err(|error| said(&error)),
                });
            }
            ArmFocus::Tag { repo, digest, .. } => {
                let manifest = self.source.manifest(&registry, &repo, &digest);
                self.send(ArmEvent::Manifest {
                    registry: name,
                    repo,
                    digest,
                    manifest: manifest.map_err(|error| said(&error)),
                });
            }
            // Answered above, where the vaults rather than the registries are
            // looked in.
            ArmFocus::Vault(_) => {}
        }
    }

    /// One secret's value, on the keystroke that asked for it. Nothing is kept
    /// here — not the value, not the fact that it was read — so the next press
    /// is another read and the thread holds no secret between them.
    fn reveal(&self, vault: &str, name: &str) {
        let value = match self.held.vaults.iter().find(|held| held.name == vault) {
            Some(held) => self
                .source
                .secret_value(held, name)
                .map_err(|error| said(&error)),
            None => Err(format!("{vault} is not one of the vaults read")),
        };
        self.send(ArmEvent::Revealed {
            vault: vault.to_owned(),
            name: name.to_owned(),
            value,
        });
    }

    /// One registry's catalog, then one attributes call per repository in it,
    /// each answered on its own so the table fills in as they land.
    // ponytail: the first refusal ends the loop. A registry that will not mint
    // a token refuses the same way for every repository in it, and asking four
    // hundred times to find that out is worse than stopping on the first.
    fn read_catalog(&mut self, registry: &Registry) {
        let listed = self.source.repositories(registry);
        let names: Vec<String> = listed
            .as_ref()
            .map(|repositories| {
                repositories
                    .iter()
                    .map(|repository| repository.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        self.send(ArmEvent::Repositories {
            registry: registry.name.clone(),
            repositories: listed.map_err(|error| said(&error)),
        });
        for repo in names {
            let read = self.source.repository(registry, &repo);
            let refused = read.is_err();
            self.send(ArmEvent::Repository {
                registry: registry.name.clone(),
                repository: read.map_err(|error| said(&error)),
            });
            if refused {
                return;
            }
        }
    }

    fn send(&self, event: ArmEvent) {
        let _ = self.events.send(event);
    }
}

/// One refusal as a line, the way every worker here reports one.
fn said(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

/// The handle the main thread holds: requests in, events out.
pub struct ArmHandle {
    requests: Sender<ArmRequest>,
    events: Receiver<ArmEvent>,
    stopped: Cell<bool>,
}

impl ArmHandle {
    /// Starts the worker on its own thread. `None` leaves the subscription for
    /// the thread to resolve, which is the `az account show` a run only pays
    /// for once one of these tabs is opened.
    pub fn spawn(config: Option<ArmConfig>) -> Result<Self> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("ticket-arm".into())
            .spawn(move || work(config, &event_sender, &request_receiver))
            .context("failed to start the Azure subscription worker")?;
        Ok(Self {
            requests: request_sender,
            events: event_receiver,
            stopped: Cell::new(false),
        })
    }

    /// Tells the worker what is worth reading. Fails only when it is gone.
    pub fn send(&self, request: ArmRequest) -> Result<()> {
        self.requests
            .send(request)
            .context("the Azure subscription worker stopped")
    }

    /// The next event, if one is waiting.
    pub fn try_event(&self) -> Option<ArmEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                (!self.stopped.replace(true)).then_some(ArmEvent::Stopped)
            }
        }
    }
}

/// The thread: settle on a subscription, then watch it.
fn work(config: Option<ArmConfig>, events: &Sender<ArmEvent>, requests: &Receiver<ArmRequest>) {
    let config = match config {
        Some(config) => config,
        None => {
            let resolved = ArmConfig::resolve(None, None).map_err(|error| said(&error));
            let _ = events.send(ArmEvent::Subscription(
                resolved
                    .as_ref()
                    .map(|config| config.subscription.clone())
                    .map_err(Clone::clone),
            ));
            match resolved {
                Ok(config) => config,
                Err(reason) => return refuse(&reason, events, requests),
            }
        }
    };
    watch(
        ArmWatcher::new(Box::new(ArmClient::new(config)), events.clone()),
        requests,
    );
}

/// A thread with no subscription to read. It stays for the run so a refresh
/// says why rather than reporting a worker that has gone, and says it once:
/// every read after the first would say the same thing.
fn refuse(reason: &str, events: &Sender<ArmEvent>, requests: &Receiver<ArmRequest>) {
    let mut said = false;
    while let Ok(request) = requests.recv() {
        match request {
            ArmRequest::Stop => return,
            ArmRequest::TabShowing(None) | ArmRequest::Blur => {}
            _ => {
                if !std::mem::replace(&mut said, true)
                    && events.send(ArmEvent::Failed(reason.to_owned())).is_err()
                {
                    return;
                }
            }
        }
    }
}

/// The loop: read whatever is due, then wait until the next thing is or a
/// request arrives, whichever comes first.
fn watch(mut watcher: ArmWatcher, requests: &Receiver<ArmRequest>) {
    loop {
        watcher.poll(Instant::now());
        let wait = watcher
            .until_due(Instant::now())
            .unwrap_or(Duration::from_secs(3600));
        match requests.recv_timeout(wait) {
            Ok(request) => {
                if !watcher.handle(request) {
                    return;
                }
                // Everything else waiting is taken now, so a burst of requests
                // costs one poll rather than one each.
                while let Ok(request) = requests.try_recv() {
                    if !watcher.handle(request) {
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arm::tests::FakeArm;
    use crate::arm::{ItemKind, Vault};
    use crate::timestamp::ts;

    fn registry(name: &str) -> Registry {
        Registry {
            id: format!(
                "/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.ContainerRegistry/registries/{name}"
            ),
            name: name.to_owned(),
            resource_group: "rg".to_owned(),
            location: "westeurope".to_owned(),
            sku: "Premium".to_owned(),
            login_server: format!("{name}.azurecr.io"),
        }
    }

    fn repository(name: &str) -> Repository {
        Repository {
            name: name.to_owned(),
            tags: Some(3),
            manifests: Some(2),
            updated: Some(ts("2026-08-29T09:00:00Z")),
        }
    }

    fn watcher(fake: &FakeArm) -> (ArmWatcher, Receiver<ArmEvent>) {
        let (sender, receiver) = mpsc::channel();
        (ArmWatcher::new(Box::new(fake.clone()), sender), receiver)
    }

    fn drain(receiver: &Receiver<ArmEvent>) -> Vec<ArmEvent> {
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    /// A one-word name for each event, which is what the order is asserted on.
    fn named(events: &[ArmEvent]) -> Vec<String> {
        events
            .iter()
            .map(|event| match event {
                ArmEvent::Subscription(_) => "subscription".to_owned(),
                ArmEvent::Inventory(_) => "inventory".to_owned(),
                ArmEvent::Repositories { registry, .. } => format!("repositories {registry}"),
                ArmEvent::Repository { repository, .. } => format!(
                    "repository {}",
                    repository
                        .as_ref()
                        .map_or_else(Clone::clone, |held| held.name.clone())
                ),
                ArmEvent::Tags { repo, .. } => format!("tags {repo}"),
                ArmEvent::Manifest { digest, .. } => format!("manifest {digest}"),
                ArmEvent::Items { vault, .. } => format!("items {vault}"),
                ArmEvent::Revealed { name, .. } => format!("revealed {name}"),
                ArmEvent::Throttled(wait) => format!("throttled {}", wait.as_secs()),
                ArmEvent::Failed(reason) => format!("failed {reason}"),
                ArmEvent::Stopped => "stopped".to_owned(),
            })
            .collect()
    }

    fn vault(name: &str) -> Vault {
        Vault {
            id: format!(
                "/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/{name}"
            ),
            name: name.to_owned(),
            resource_group: "rg".to_owned(),
            location: "westeurope".to_owned(),
            sku: "standard".to_owned(),
            uri: format!("https://{name}.vault.azure.net/"),
        }
    }

    fn stocked() -> FakeArm {
        let fake = FakeArm::default();
        *fake.registries.lock().unwrap() = vec![registry("acr")];
        *fake.repositories.lock().unwrap() = vec![repository("team/api"), repository("team/web")];
        *fake.tags.lock().unwrap() = vec![Tag {
            name: "latest".to_owned(),
            digest: "sha256:aaa".to_owned(),
            created: Some(ts("2026-08-29T09:00:00Z")),
            updated: None,
        }];
        fake
    }

    #[test]
    fn the_inventory_is_read_once_a_minute_and_only_while_an_arm_tab_shows() {
        let fake = stocked();
        let (mut watcher, receiver) = watcher(&fake);
        let start = Instant::now();

        watcher.poll(start);
        assert!(
            fake.reads.lock().unwrap().is_empty(),
            "neither tab is showing, so nothing is read"
        );
        assert_eq!(watcher.until_due(start), None);

        watcher.handle(ArmRequest::TabShowing(Some(TabId::Acr)));
        assert_eq!(watcher.until_due(start), Some(Duration::ZERO));
        watcher.poll(start);
        assert_eq!(named(&drain(&receiver)), vec!["inventory".to_owned()]);
        assert_eq!(watcher.until_due(start), Some(INVENTORY_CADENCE));

        // Well inside the cadence: nothing is asked again.
        watcher.poll(start + Duration::from_secs(30));
        assert!(drain(&receiver).is_empty());
        watcher.poll(start + INVENTORY_CADENCE);
        assert_eq!(named(&drain(&receiver)), vec!["inventory".to_owned()]);

        // The Key Vault tab keeps the same thread reading the same inventory.
        watcher.handle(ArmRequest::TabShowing(Some(TabId::KeyVault)));
        watcher.handle(ArmRequest::Refresh);
        watcher.poll(start + INVENTORY_CADENCE);
        assert_eq!(named(&drain(&receiver)), vec!["inventory".to_owned()]);

        watcher.handle(ArmRequest::TabShowing(None));
        assert_eq!(watcher.until_due(start + INVENTORY_CADENCE), None);
        assert!(!watcher.handle(ArmRequest::Stop));
    }

    #[test]
    fn a_focus_reads_the_catalog_then_every_repositorys_attributes_and_only_once() {
        let fake = stocked();
        let (mut watcher, receiver) = watcher(&fake);
        let start = Instant::now();
        watcher.handle(ArmRequest::TabShowing(Some(TabId::Acr)));
        watcher.handle(ArmRequest::Focus(ArmFocus::Registry("acr".to_owned())));
        watcher.poll(start);

        assert_eq!(
            named(&drain(&receiver)),
            vec![
                "inventory".to_owned(),
                "repositories acr".to_owned(),
                "repository team/api".to_owned(),
                "repository team/web".to_owned(),
            ]
        );

        // The same focus again costs nothing.
        watcher.handle(ArmRequest::Focus(ArmFocus::Registry("acr".to_owned())));
        watcher.poll(start + Duration::from_secs(1));
        assert!(drain(&receiver).is_empty());

        // Down into one repository, and then into one tag.
        watcher.handle(ArmRequest::Focus(ArmFocus::Repository {
            registry: "acr".to_owned(),
            name: "team/api".to_owned(),
        }));
        watcher.poll(start + Duration::from_secs(2));
        assert_eq!(named(&drain(&receiver)), vec!["tags team/api".to_owned()]);

        watcher.handle(ArmRequest::Focus(ArmFocus::Tag {
            registry: "acr".to_owned(),
            repo: "team/api".to_owned(),
            digest: "sha256:aaa".to_owned(),
        }));
        watcher.poll(start + Duration::from_secs(3));
        assert_eq!(
            named(&drain(&receiver)),
            vec!["manifest sha256:aaa".to_owned()]
        );

        // Nothing on screen worth reading, and the next poll asks for nothing.
        watcher.handle(ArmRequest::Blur);
        watcher.poll(start + Duration::from_secs(4));
        assert!(drain(&receiver).is_empty());

        // A refresh reads it all over again.
        watcher.handle(ArmRequest::Focus(ArmFocus::Registry("acr".to_owned())));
        watcher.handle(ArmRequest::Refresh);
        watcher.poll(start + Duration::from_secs(5));
        assert_eq!(
            named(&drain(&receiver)),
            vec![
                "inventory".to_owned(),
                "repositories acr".to_owned(),
                "repository team/api".to_owned(),
                "repository team/web".to_owned(),
            ]
        );
    }

    #[test]
    fn a_throttled_read_holds_the_next_one_off_for_as_long_as_azure_asked() {
        let fake = stocked();
        // Longer than the cadence, which is the only case where the wait
        // changes anything: a shorter one is already served by waiting a
        // minute.
        *fake.throttle.lock().unwrap() = Some(Duration::from_secs(90));
        let (mut watcher, receiver) = watcher(&fake);
        let start = Instant::now();
        watcher.handle(ArmRequest::TabShowing(Some(TabId::Acr)));
        watcher.poll(start);

        assert_eq!(
            named(&drain(&receiver)),
            vec!["inventory".to_owned(), "throttled 90".to_owned()]
        );
        assert_eq!(
            watcher.until_due(start),
            Some(Duration::from_secs(90)),
            "the wait Azure asked for wins over the cadence"
        );
        // And it is not read again until that wait is up.
        watcher.poll(start + INVENTORY_CADENCE);
        assert!(drain(&receiver).is_empty());
    }

    #[test]
    fn a_failing_read_is_reported_and_stretches_the_cadence() {
        let fake = stocked();
        *fake.failure.lock().unwrap() = Some("az login first".to_owned());
        let (mut watcher, receiver) = watcher(&fake);
        let start = Instant::now();
        watcher.handle(ArmRequest::TabShowing(Some(TabId::Acr)));
        watcher.poll(start);

        let events = drain(&receiver);
        assert!(
            matches!(events.as_slice(), [ArmEvent::Inventory(Err(reason))] if reason.contains("az login first")),
            "{:?}",
            named(&events)
        );
        assert_eq!(
            watcher.until_due(start),
            Some(INVENTORY_CADENCE),
            "a minute is already the longest any cadence stretches to"
        );
    }

    #[test]
    fn the_first_repository_that_refuses_ends_the_run_over_the_catalog() {
        let fake = stocked();
        let (mut watcher, receiver) = watcher(&fake);
        let start = Instant::now();
        watcher.handle(ArmRequest::TabShowing(Some(TabId::Acr)));
        watcher.poll(start);
        drain(&receiver);

        *fake.repository_failure.lock().unwrap() = Some("no metadata_read scope".to_owned());
        watcher.handle(ArmRequest::Focus(ArmFocus::Registry("acr".to_owned())));
        watcher.poll(start + Duration::from_secs(1));
        let events = named(&drain(&receiver));
        assert_eq!(
            events,
            vec![
                "repositories acr".to_owned(),
                "repository no metadata_read scope".to_owned(),
            ],
            "the catalog lists two, and one refusal is enough to know the second will refuse too"
        );
    }

    #[test]
    fn a_vault_focus_lists_what_it_holds_once_and_a_poll_never_asks_for_a_value() {
        let fake = stocked();
        *fake.vaults.lock().unwrap() = vec![vault("kv")];
        *fake.items.lock().unwrap() = vec![VaultItem {
            kind: ItemKind::Secret,
            name: "db-password".to_owned(),
            enabled: true,
            created: None,
            updated: None,
            expires: None,
            content_type: None,
            recovery_level: None,
        }];
        let (mut watcher, receiver) = watcher(&fake);
        let start = Instant::now();
        watcher.handle(ArmRequest::TabShowing(Some(TabId::KeyVault)));
        watcher.handle(ArmRequest::Focus(ArmFocus::Vault("kv".to_owned())));
        watcher.poll(start);

        assert_eq!(
            named(&drain(&receiver)),
            vec!["inventory".to_owned(), "items kv".to_owned()]
        );

        // The same focus again, and every poll after it, costs nothing.
        watcher.handle(ArmRequest::Focus(ArmFocus::Vault("kv".to_owned())));
        watcher.poll(start + Duration::from_secs(1));
        watcher.poll(start + INVENTORY_CADENCE);
        assert_eq!(
            named(&drain(&receiver)),
            vec!["inventory".to_owned()],
            "the cadence reads the subscription, never a vault's items again"
        );

        // A value is only ever read when something asks for one in as many
        // words, and the answer says nothing a log could keep.
        watcher.handle(ArmRequest::Reveal {
            vault: "kv".to_owned(),
            name: "db-password".to_owned(),
        });
        let events = drain(&receiver);
        assert_eq!(named(&events), vec!["revealed db-password".to_owned()]);
        let ArmEvent::Revealed {
            value: Ok(secret), ..
        } = &events[0]
        else {
            panic!("the reveal answered with {:?}", named(&events));
        };
        assert_eq!(secret.expose(), "");
        let printed = format!("{:?}", events[0]);
        assert!(printed.contains("[redacted]"), "{printed}");

        // And nothing is held: the next poll asks for nothing at all.
        watcher.poll(start + INVENTORY_CADENCE + Duration::from_secs(1));
        assert!(drain(&receiver).is_empty());
        assert_eq!(
            fake.reads
                .lock()
                .unwrap()
                .iter()
                .filter(|read| read.starts_with("secret_value"))
                .count(),
            1,
            "one keystroke, one read"
        );
    }

    #[test]
    fn a_revealed_value_never_prints_itself_wherever_the_event_is_written() {
        let fake = stocked();
        *fake.vaults.lock().unwrap() = vec![vault("kv")];
        *fake.secret.lock().unwrap() = "hunter2".to_owned();
        let (mut watcher, receiver) = watcher(&fake);
        watcher.handle(ArmRequest::TabShowing(Some(TabId::KeyVault)));
        watcher.poll(Instant::now());
        drain(&receiver);

        watcher.handle(ArmRequest::Reveal {
            vault: "kv".to_owned(),
            name: "db-password".to_owned(),
        });
        let events = drain(&receiver);
        let printed = format!("{:?}", events[0]);
        assert!(printed.contains("[redacted]"), "{printed}");
        assert!(
            !printed.contains("hunter2"),
            "the one thing worth hiding is the one thing Debug must not print: {printed}"
        );

        // A vault the inventory does not hold is refused rather than read.
        watcher.handle(ArmRequest::Reveal {
            vault: "nowhere".to_owned(),
            name: "db-password".to_owned(),
        });
        let events = drain(&receiver);
        assert!(
            matches!(&events[0], ArmEvent::Revealed { value: Err(reason), .. } if reason.contains("nowhere")),
            "{:?}",
            named(&events)
        );
    }

    /// Whatever the worker says within the deadline, or nothing.
    fn await_event(handle: &ArmHandle, seconds: u64) -> Option<ArmEvent> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            match handle.try_event() {
                Some(event) => return Some(event),
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
        None
    }

    #[test]
    fn the_handle_runs_the_worker_on_its_own_thread_and_says_once_when_it_stops() {
        // A configured subscription mints nothing until something is read, and
        // with neither tab showing nothing is: this never leaves the process.
        let handle = ArmHandle::spawn(Some(ArmConfig {
            subscription: "sub-1".to_owned(),
        }))
        .unwrap();
        handle.send(ArmRequest::TabShowing(None)).unwrap();
        handle.send(ArmRequest::Stop).unwrap();
        assert!(matches!(await_event(&handle, 5), Some(ArmEvent::Stopped)));
        assert!(handle.try_event().is_none(), "Stopped is said once");
    }

    #[test]
    fn a_thread_given_no_subscription_settles_one_of_its_own_off_the_startup_path() {
        // The one place `az account show` runs. Whether this machine has a
        // signed-in CLI or none at all, the answer comes back as an event
        // rather than as a startup failure.
        let handle = ArmHandle::spawn(None).unwrap();
        match await_event(&handle, 30) {
            Some(ArmEvent::Subscription(Ok(subscription))) => {
                assert!(!subscription.trim().is_empty());
            }
            // No CLI, or one nobody has signed in to: the thread stays, and
            // says why once rather than answering a read it cannot make.
            Some(ArmEvent::Subscription(Err(reason))) => {
                assert!(!reason.is_empty());
                handle.send(ArmRequest::Refresh).unwrap();
                assert!(matches!(await_event(&handle, 5), Some(ArmEvent::Failed(_))));
            }
            other => panic!("the thread answered with {other:?}"),
        }
        handle.send(ArmRequest::Stop).unwrap();
    }
}
