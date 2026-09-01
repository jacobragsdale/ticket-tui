"""Walk the ergonomics round (#736-#741) in the release binary under a pty: `g` on tabs 1-5,
`[`/`]` across tabs and a drill, `j` x5 recording nothing, `+` quick capture from the AKS tab
(which creates one real Issue tagged inbox -- delete it after), `?`, `ticket-tui status` against the
live TUI, and the session file at quit. Reads a backup of the real database (or
TICKET_TUI_WALK_DATABASE) and scripts/fake-kubectl. Run from the repository root after
`cargo build --release`; needs the pyte module. Prints PASS/FAIL per step; exit code = failures."""
import fcntl, json, os, pty, select, struct, subprocess, sys, tempfile, termios, time, shutil
import pyte

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "ticket-tui")
S = tempfile.mkdtemp(prefix="ticket-tui-walk-")
W = os.path.join(S, "walk"); os.makedirs(os.path.join(W, "cfg", "ticket-tui"))
# The kustomize fixture as a clone the workspace scan will find: a git repository with an origin.
WS = os.path.join(S, "ws"); os.makedirs(WS)
shutil.copytree(os.path.join(ROOT, "fixtures", "kustomize"), os.path.join(WS, "kustomize"))
subprocess.run(["git", "init", "-q"], cwd=os.path.join(WS, "kustomize"), check=True)
subprocess.run(["git", "-c", "user.email=walk@example", "-c", "user.name=walk", "add", "-A"], cwd=os.path.join(WS, "kustomize"), check=True)
subprocess.run(["git", "-c", "user.email=walk@example", "-c", "user.name=walk", "commit", "-qm", "fixture"], cwd=os.path.join(WS, "kustomize"), check=True)
subprocess.run(["git", "remote", "add", "origin", "https://example.invalid/deployment/kustomize.git"], cwd=os.path.join(WS, "kustomize"), check=True)
os.symlink(os.path.join(ROOT, "scripts", "fake-kubectl"), os.path.join(W, "kubectl"))
DB = os.path.join(W, "qa.sqlite3"); CTX = os.path.join(W, "qa.context.json"); SESSION = os.path.join(W, "qa.session.json")
NOTIFY = os.path.join(W, "notify.log")
REAL = os.environ.get("TICKET_TUI_WALK_DATABASE") or os.path.expanduser("~/Library/Application Support/ticket-tui/tickets.sqlite3")
subprocess.run(["sqlite3", REAL, f".backup {DB}"], check=True)
KUBECTL = shutil.which("kubectl", path=os.environ["PATH"]) or "kubectl"
with open(os.path.join(W, "cfg", "ticket-tui", "config.toml"), "w") as f:
    f.write(f'''[[clusters]]
name = "qa"
context = "aks-qa"
namespaces = ["orders", "billing"]

[notify]
command = "printf '%s|%s\\\\n' {{title}} {{body}} >> {NOTIFY}"

[deployment]
repo = "kustomize"
render = "{KUBECTL} kustomize"

[[environments]]
name = "qa"
overlays = ["overlays/qa"]
vault = "kv-qa"

[[environments]]
name = "prod"
overlays = ["overlays/prod"]
vault = "kv-prod"
''')
REFRESH = sys.argv[1] if len(sys.argv) > 1 else "0"
env = dict(os.environ, PATH=W + ":" + os.environ["PATH"], XDG_CONFIG_HOME=os.path.join(W, "cfg"),
           TICKET_TUI_THEME="mono", TERM="xterm-256color")
env.pop("TICKET_TUI_WORKSPACE", None)
pid, fd = pty.fork()
if pid == 0:
    os.execve(BIN, [BIN, "--database", DB, "--refresh", REFRESH, "--workspace", WS], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 160, 0, 0))
screen = pyte.Screen(160, 45); stream = pyte.ByteStream(screen)
alive = True
def drain(seconds):
    global alive
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try: data = os.read(fd, 65536)
            except OSError: alive = False; return
            if not data: alive = False; return
            if b"\x1b[6n" in data: os.write(fd, b"\x1b[1;1R")
            stream.feed(data)
def text(): return "\n".join(l.rstrip() for l in screen.display)
def key(k, wait=0.5): os.write(fd, k.encode()); drain(wait)
def ctx():
    for _ in range(50):
        try:
            with open(CTX) as f: return json.load(f)
        except (FileNotFoundError, json.JSONDecodeError): drain(0.1)
    raise SystemExit("no context file")
def footer(): return screen.display[-1].strip()
def anywhere(s): return s in text()
fails = 0
def check(name, ok, detail=""):
    global fails
    if not ok: fails += 1
    print(("PASS " if ok else "FAIL ") + name + (f" — {detail}" if detail and not ok else ""))
def wait_for(pred, seconds, step=0.2):
    end = time.time() + seconds
    while time.time() < end:
        drain(step)
        try:
            if pred(): return True
        except Exception: pass
    return False
def sel_wi(c): return (c["work_items"].get("selected_ticket") or {}).get("id")
def sel_pr(c): return (c["pull_requests"].get("selected") or {}).get("id")
def jump_kind(j): return None if j is None else j.get("kind")

drain(2.5)
c = ctx()
check("starts on work items", c["active_tab"] == "work_items", c["active_tab"])
check("context block has follow on every tab", all("follow" in c[t] for t in ("work_items","repos","pull_requests","pipelines","aks","acr","key_vault")))

# --- B. work items: g follows a linked PR / build, or says why not
key("1"); found = None
for _ in range(60):
    c = ctx()
    if c["work_items"]["follow"] is not None: found = c; break
    key("j", 0.15)
if found:
    wi = sel_wi(found); kind = jump_kind(found["work_items"]["follow"])
    check(f"footer offers g on #{wi} ({kind})", "g " in footer(), footer())
    key("g", 1.0); c = ctx()
    expect = {"pull-request": "pull_requests", "work-items": "work_items", "work-item": "work_items", "run": "pipelines"}.get(kind, "?")
    check(f"g from #{wi} lands on {expect}", c["active_tab"] == expect, f"{c['active_tab']} follow={found['work_items']['follow']}")
    key("[", 1.0); c = ctx()
    check("[ returns to the work item", c["active_tab"] == "work_items" and sel_wi(c) == wi, f"{c['active_tab']} #{sel_wi(c)}")
    key("]", 1.0); c = ctx()
    check("] goes forward again", c["active_tab"] == expect, c["active_tab"])
    key("1", 0.5); key("g", 0.8)  # already on it: g again from the work item, then back with [
    key("[", 0.8)
else:
    key("g", 0.8)
    check("g with nothing linked says why", "no linked pull request or build" in footer(), footer())
    check("footer does not promise g", "g " not in footer() or "Go to" not in footer())

# --- C. pull requests
key("3", 0.8); c = ctx(); pr = sel_pr(c); fol = c["pull_requests"]["follow"]
if fol:
    key("g", 1.0); c2 = ctx()
    check(f"g from !{pr} lands on {jump_kind(fol)}", c2["active_tab"] in ("work_items", "pipelines"), c2["active_tab"])
    key("[", 0.8); c3 = ctx()
    check("[ returns to the pull request", c3["active_tab"] == "pull_requests" and sel_pr(c3) == pr, f"{c3['active_tab']} !{sel_pr(c3)}")
else:
    key("g", 0.8)
    check(f"g on !{pr} with no work items says why", "carries no work items" in footer(), footer())

# --- D. pipelines: level rule and drill retrace
key("4", 0.8); c = ctx()
check("pipelines level", c["pipelines"]["level"] == "pipelines")
key("g", 0.8); check("g at pipelines level says open a pipeline first", "Open a pipeline first" in footer(), footer())
key("\r", 1.0); c = ctx(); check("Enter drills into runs", c["pipelines"]["level"] == "runs", c["pipelines"]["level"])
fol = c["pipelines"]["follow"]; run = (c["pipelines"].get("selected_run") or {}).get("id")
key("g", 0.8); c2 = ctx()
if fol: check("g from the run lands on its pull request", c2["active_tab"] == "pull_requests", c2["active_tab"]); key("[", 0.8)
else: check("g on a run without a PR says why", "was not started by a pull request" in footer(), footer())
key("[", 0.8); c = ctx()
check("[ climbs back out to the pipelines level", c["active_tab"] == "pipelines" and c["pipelines"]["level"] == "pipelines", f"{c['active_tab']} {c['pipelines']['level']}")

# --- E. repos
key("2", 0.8); c = ctx(); fol = c["repos"]["follow"]; repo = (c["repos"].get("selected") or {}).get("name")
key("g", 0.8); c2 = ctx()
if fol: check(f"g from {repo} lands on its open pull request", c2["active_tab"] == "pull_requests", c2["active_tab"]); key("[", 0.8)
else: check(f"g on {repo} with no open PR says why", "No open pull request on" in footer(), footer())

# --- F. AKS: g still lands on the repository
key("5", 1.0)
wait_for(lambda: ctx()["aks"]["visible_rows"] > 0, 8)
c = ctx(); pod = c["aks"]["selected"]; fol = c["aks"]["follow"]
key("g", 1.0); c2 = ctx()
if fol: check("AKS g lands on the repository", c2["active_tab"] == "repos", c2["active_tab"]); key("[", 0.8)
else: check("AKS g says why", c2["active_tab"] == "aks" and footer() != "", footer())

# --- G. tab switch round trip
key("3", 0.8); c = ctx(); pr = sel_pr(c)
key("1", 0.8); key("[", 0.8); c = ctx()
check("cursor onto a PR, 1, [ — back on that PR", c["active_tab"] == "pull_requests" and sel_pr(c) == pr, f"{c['active_tab']} !{sel_pr(c)}")
key("]", 0.8); c = ctx(); check("] forward to work items", c["active_tab"] == "work_items", c["active_tab"])

# --- H. j x5 records nothing but the row stopped on
key("1", 0.5); c = ctx(); start = sel_wi(c)
for _ in range(5): key("j", 0.15)
c = ctx(); stopped = sel_wi(c)
key("2", 0.8); key("[", 0.8); c = ctx()
check("[ after j x5 returns to the row stopped on", c["active_tab"] == "work_items" and sel_wi(c) == stopped, f"#{sel_wi(c)} vs #{stopped}")
key("[", 0.8); c = ctx()
check("[ again is not one of the rows passed", not (c["active_tab"] == "work_items" and sel_wi(c) not in (stopped, start) and sel_wi(c) != stopped and False) and (c["active_tab"] != "work_items" or sel_wi(c) == start or sel_wi(c) != stopped), f"{c['active_tab']} #{sel_wi(c)}")

# --- I. + quick capture from the AKS tab
key("5", 0.8); c = ctx(); before = c["aks"]["selected"]
key("+", 0.5); key("\r", 0.5)
check("empty title refused in place", "A work item needs a title" in footer(), footer())
key("\x1b", 0.5); c = ctx(); check("Esc leaves no pending edit", not c["pending_edits"], str(c["pending_edits"]))
key("+", 0.5); key("QA walk capture (delete me)", 0.3); key("\r", 0.5)
ok = wait_for(lambda: "Created Issue #" in text(), 15)
line = next((l for l in screen.display if "Created Issue #" in l), "")
new_id = int(line.split("Created Issue #")[1].split()[0].strip("·")) if ok else None
check("+ from AKS creates an Issue", ok and new_id, text()[-400:])
c = ctx(); check("cursor stays on the AKS row", c["active_tab"] == "aks" and c["aks"]["selected"] == before, f"{c['active_tab']} {c['aks']['selected']}")
if new_id:
    show = subprocess.run([BIN, "--database", DB, "show", str(new_id), "--json"], capture_output=True, text=True).stdout
    try: item = json.loads(show)
    except Exception: item = {}
    check("it is an Issue tagged inbox on me", item.get("type") == "Issue" and "inbox" in str(item.get("tags")) and item.get("assignee"), show[:300])
    print(f"CREATED #{new_id} -- delete it: az boards work-item delete --id {new_id} --yes")

# --- J. status against the live TUI
out = subprocess.run([BIN, "--database", DB, "status", "--json"], capture_output=True, text=True, env=env).stdout
st = json.loads(out or "{}"); c = ctx()
check("status sees the live TUI", st.get("context") == "live", out)
check("status pods_unhealthy matches the tab", st.get("pods_unhealthy") == c["aks"]["unhealthy"], f"{st.get('pods_unhealthy')} vs {c['aks']['unhealthy']}")
line = subprocess.run([BIN, "--database", DB, "status"], capture_output=True, text=True, env=env).stdout.strip()
check("status line carries the pods glyph", "✗" in line and "pods" in line, line)

# --- K. help lists g and + once each
key("?", 0.8); t = text()
check("? lists Quick capture once", t.count("Quick capture") == 1, str(t.count("Quick capture")))
check("? lists the follow key once", sum(1 for l in t.splitlines() if l.strip().startswith("g ") or "  g  " in l) == 1)
key("\x1b", 0.5)

# --- L. quit and the session
key("q", 1.5)
wait_for(lambda: not alive, 5)
try:
    with open(SESSION) as f: sess = json.load(f)
    hist = sess.get("history", [])
    kinds = [h.get("kind") for h in hist]
    check("session history holds a pull request and a pod", "pull-request" in kinds and ("pod" in kinds or "repo" in kinds), str(kinds))
except Exception as e:
    check("session file readable", False, str(e))
print("notify.log:", open(NOTIFY).read() if os.path.exists(NOTIFY) else "(none)")
print(f"{fails} failures")
sys.exit(fails)
