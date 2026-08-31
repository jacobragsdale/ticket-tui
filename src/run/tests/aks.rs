//! The cluster thread as the run drives it: what it is told, and what it sends
//! back. A real worker over a fake `kubectl`.

use std::sync::{Arc, Mutex};

use super::*;
use ticket_tui::aks::{Container, KubeSource, LogTail, Pod, PodKey};

/// One cluster stood in for: one pod per read, two lines of log, and a note of
/// what it was asked for.
#[derive(Clone, Default)]
struct FakeKube {
    reads: Arc<Mutex<Vec<String>>>,
    follows: Arc<Mutex<Vec<LogFollow>>>,
}

impl KubeSource for FakeKube {
    fn pods(&self, cluster: &Cluster, namespace: Option<&str>) -> Result<Vec<Pod>> {
        self.reads.lock().unwrap().push(cluster.name.clone());
        Ok(vec![Pod {
            key: PodKey {
                cluster: cluster.name.clone(),
                namespace: namespace.unwrap_or("orders").to_owned(),
                name: "orders-api-7d9f5b-abc12".to_owned(),
            },
            status: "Running".to_owned(),
            ready: (1, 1),
            restarts: 0,
            created: None,
            node: "aks-nodepool1-0".to_owned(),
            ip: "10.0.0.7".to_owned(),
            owner: None,
            containers: vec![Container {
                name: "api".to_owned(),
                image: "myacr.azurecr.io/team/orders-api:1.2.3".to_owned(),
                ready: true,
                restarts: 0,
                state: "Running".to_owned(),
                last_termination: None,
            }],
            labels: Vec::new(),
        }])
    }

    fn logs(&self, _cluster: &Cluster, target: &LogFollow) -> Result<LogTail> {
        self.follows.lock().unwrap().push(target.clone());
        Ok(LogTail {
            child: None,
            stdout: Box::new(std::io::Cursor::new(
                "2026-08-30T10:00:00Z starting\n2026-08-30T10:00:01Z listening\n".to_owned(),
            )),
            stderr: None,
        })
    }
}

/// A run with the cluster worker and nothing else: no database is touched by
/// any of this.
fn aks_runtime(source: FakeKube) -> SyncRuntime {
    SyncRuntime {
        worker: None,
        scheduler: SyncScheduler::new(None),
        config: None,
        offline_reason: None,
        details: DetailsEngine::default(),
        pipelines: None,
        watching_tab: false,
        watching_run: (None, None),
        watched_runs: Vec::new(),
        local: LocalRuntime::default(),
        aks: AksRuntime {
            worker: Some(AksHandle::spawn(Box::new(source)).unwrap()),
            ..AksRuntime::default()
        },
        arm_config: None,
    }
}

#[test]
fn the_aks_thread_starts_with_the_first_cluster_and_hears_when_the_tab_shows_and_what_is_followed()
{
    let fake = FakeKube::default();
    let mut runtime = aks_runtime(fake.clone());
    let mut app = App::new(Vec::new());
    app.aks.set_clusters(vec![Cluster {
        name: "qa".to_owned(),
        context: "aks-qa".to_owned(),
        namespaces: vec!["orders".to_owned()],
    }]);

    // The clusters go out at once; nothing is read while the tab is hidden.
    poll_aks(&mut app, &mut runtime);
    assert_eq!(runtime.aks.clusters.len(), 1);
    assert!(!runtime.aks.showing);
    assert_eq!(app.aks.pod_count(), 0);

    app.select_tab(TabId::Aks);
    poll_aks(&mut app, &mut runtime);
    assert!(runtime.aks.showing, "the worker hears the tab is showing");

    // The read lands, the pod under the cursor is followed, and its lines
    // arrive on the screen — one event each, so both are waited for.
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.aks.log_lines().len() < 2 {
        poll_aks(&mut app, &mut runtime);
        assert!(Instant::now() < deadline, "the cluster worker timed out");
        thread::yield_now();
    }
    assert_eq!(app.aks.pod_count(), 1);
    assert!(
        !fake.reads.lock().unwrap().is_empty(),
        "the cluster was read once the tab showed"
    );
    let follows = fake.follows.lock().unwrap().clone();
    assert_eq!(follows.len(), 1, "one stream, for the pod on screen");
    assert_eq!(follows[0].key.name, "orders-api-7d9f5b-abc12");
    assert_eq!(
        runtime.aks.following.as_ref().map(|target| &target.key),
        Some(&follows[0].key),
        "and the run remembers what it asked for, so it asks once"
    );
    assert_eq!(
        app.aks.log_lines(),
        [
            "2026-08-30T10:00:00Z starting",
            "2026-08-30T10:00:01Z listening"
        ]
    );
}
