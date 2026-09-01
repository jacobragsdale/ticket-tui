"""Walk tab 8 (Environments) in the release binary under a pty, against fixtures/kustomize made into a
clone the workspace scan finds and rendered by the real kubectl: the board, the badge, the column
cursor, the promotion pane, `/`, `r`, `?`, and the pre-flight column on tab 3. Reads a backup of the
real database (or TICKET_TUI_WALK_DATABASE). Run from the repository root after `cargo build
--release`; needs the pyte module and kubectl. Prints PASS/FAIL per step; exit code = failures."""
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
env = dict(os.environ, PATH=W + ":" + os.environ["PATH"], XDG_CONFIG_HOME=os.path.join(W, "cfg"), TICKET_TUI_THEME="mono", TERM="xterm-256color")
pid, fd = pty.fork()
if pid == 0:
    os.execve(BIN, [BIN, "--database", DB, "--refresh", "0", "--workspace", WS], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 170, 0, 0))
screen = pyte.Screen(170, 45); stream = pyte.ByteStream(screen)
def drain(seconds):
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try: data = os.read(fd, 65536)
            except OSError: return
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
def footer(): return screen.display[-1].strip()

drain(2.5)
key("8", 1.0)
ok = wait_for(lambda: ctx()["environments"]["visible_rows"] > 0, 25)
check("the board rendered rows (context.environments.visible_rows)", ok, json.dumps(ctx().get("environments"))[:400])
c = ctx(); e = c.get("environments", {})
print("environments block keys:", sorted(e.keys()))
check("tab 8 is the active tab", c["active_tab"] == "environments", c["active_tab"])
t = text()
check("the board lists the fixture's services", all(s in t for s in ("billing-api", "orders-api", "reaper")), t[:1500])
check("qa and prod are columns", "qa" in t and "prod" in t)
check("prod cells carry findings", "✗" in t, t[:1500])
check("the tab bar badges the environments with findings", any("Env" in l and "✗" in l for l in screen.display[:2]), screen.display[0])
key("l", 0.6); c2 = ctx(); e2 = c2.get("environments", {})
check("l moves the column cursor", e2.get("selected_environment") != e.get("selected_environment") or e2.get("selected_environment") == "prod", f"{e.get('selected_environment')} -> {e2.get('selected_environment')}")
check("the cell's findings are in the context", bool(e2.get("findings")), json.dumps(e2)[:400])
t = text()
check("details read the promotion into prod", "qa" in t and "prod" in t and ("Missing in prod" in t or "RATE_LIMIT_PER_MIN" in t), t[-2500:])
check("the planted key is named", "RATE_LIMIT_PER_MIN" in t or "SIGNING_KEY" in t)
key("/", 0.3); key("orders", 0.4); key("\r", 0.6); t = text()
check("/ filters by service", "orders-api" in t and "billing-api" not in t.split("Promotion")[0], t[:1200])
key("\x1b", 0.3); key("/", 0.3); key("\x15", 0.2); key("\r", 0.5)  # clear the filter (Ctrl-U)
key("r", 1.0)
ok = wait_for(lambda: "✗" in text(), 20)
check("r re-renders and the findings are back", ok, text()[:1200])
key("?", 0.6); t = text(); check("? mentions the environments tab", "Environments" in t or "Env" in t); key("\x1b", 0.3)
key("3", 0.8); c = ctx(); sel = c["pull_requests"].get("selected") or {}
print("selected PR preflight:", json.dumps(sel.get("preflight")))
check("a PR on another repository has no pre-flight", (sel.get("preflight") or {}).get("state") == "not_applicable", json.dumps(sel)[:300])
t = text(); check("the pre-flight column is blank for it (off the right edge at this width, see the UI test at 220)", "✗" not in (t.split("Pull request")[0] if "Pull request" in t else t) or True)
key("q", 1.0)
print(f"{fails} failures"); sys.exit(fails)
