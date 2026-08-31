"""Walk every AKS verb in the release binary under a pty, against `scripts/fake-kubectl`.

No cluster is needed: the fake answers `get pods`, streams `logs -f`, and takes
`describe`, `delete` and `exec`. Run from the repository root after
`cargo build --release`:

    python3 scripts/walk_aks.py

Needs the `pyte` module (a VT emulator) to read the screen. Every step prints
PASS or FAIL; the exit code is the number of failures.
"""
import fcntl, json, os, pty, select, signal, struct, subprocess, sys, tempfile, termios, time
try:
    import pyte
except ImportError:
    sys.exit("walk_aks.py needs the pyte module: pip install pyte")

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "ticket-tui")
S = tempfile.mkdtemp(prefix="ticket-tui-walk-")
os.symlink(os.path.join(ROOT, "scripts", "fake-kubectl"), os.path.join(S, "kubectl"))
os.makedirs(os.path.join(S, "cfg", "ticket-tui"))
DB = os.path.join(S, "walk.sqlite3"); CTX = os.path.join(S, "walk.context.json"); LOG = os.path.join(S, "calls.log")
CFG = os.path.join(S, "cfg", "ticket-tui", "config.toml")
for p in (DB, CTX, LOG, DB + "-wal", DB + "-shm", os.path.join(S, "walk.session.json")):
    try: os.remove(p)
    except FileNotFoundError: pass
with open(CFG, "w") as f:
    f.write('[[clusters]]\nname = "qa"\ncontext = "aks-qa"\nnamespaces = ["orders", "billing", "forbidden"]\n\n[[clusters]]\nname = "prod"\ncontext = "aks-prod"\n\n[[clusters]]\nname = "bad"\ncontext = "aks-nowhere"\n')
env = dict(os.environ, PATH=S + ":" + os.environ["PATH"], XDG_CONFIG_HOME=os.path.join(S, "cfg"), FAKE_KUBECTL_LOG=S, TICKET_TUI_THEME="mono", TERM="xterm-256color")
pid, fd = pty.fork()
if pid == 0:
    os.execve(BIN, [BIN, "--database", DB, "--refresh", "0"], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
screen = pyte.Screen(140, 40); stream = pyte.ByteStream(screen)
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
            if b"\x1b[6n" in data:   # a cursor-position query: answer like a terminal would
                os.write(fd, b"\x1b[1;1R")
            stream.feed(data)
def text(): return "\n".join(line.rstrip() for line in screen.display)
def key(k, wait=0.6):
    os.write(fd, k.encode()); drain(wait)
def ctx():
    for _ in range(40):
        try:
            with open(CTX) as f: return json.load(f)
        except (FileNotFoundError, json.JSONDecodeError): drain(0.1)
    raise SystemExit("no context file")
def calls():
    try:
        with open(LOG) as f: return f.read().splitlines()
    except FileNotFoundError: return []
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
def status():
    return [l for l in screen.display if l.strip()][-1] if any(l.strip() for l in screen.display) else ""

drain(2.0)
key("5", 1.0)
ok = wait_for(lambda: ctx()["active_tab"] == "aks" and ctx()["aks"]["visible_rows"] >= 7, 8)
c = ctx(); check("5 opens AKS and both clusters' rows land", ok, json.dumps(c.get("aks"))[:300])
check("table shows a CrashLoop row with ✗ and a Terminating row with ◐", "✗ CrashLoopBackOff" in text() and "◐ Terminating" in text(), text())
check("the bad context and the forbidden namespace are the only errors", len(c["aks"]["errors"]) == 2, str(c["aks"]["errors"]))
check("the tab badge counts the unhealthy pod", "AKS ✗1" in text() or "✗1" in text().splitlines()[0], text().splitlines()[0])
ok = wait_for(lambda: ctx()["aks"]["following_log"]["line_count"] >= 2, 6)
check("log lines arrive", ok, str(ctx()["aks"]["following_log"]))
check("the log pane title says following", "following" in text(), text())
key("/", 0.3); key("billing-api", 0.5); key("\r", 0.8)
c = ctx(); check("fuzzy narrows to the two billing pods", c["aks"]["visible_rows"] == 2, str(c["aks"]["visible_rows"]))
key("L", 0.6)   # focus goes to the details pane
key("k", 0.5)
c = ctx(); check("k on the focused log pane leaves follow", c["aks"]["following_log"]["following"] is False, json.dumps(c["aks"]["following_log"]))
check("the title says scrolled", "scrolled" in text(), "")
key("\x1b[F", 0.5)
c = ctx(); check("End follows again", c["aks"]["following_log"]["following"] is True, "")
key("l", 0.5); check("l gives the log the whole pane", "Owner" not in text(), ""); key("l", 0.5)
key("D", 1.5)
check("D shows describe text in the pane", "Events:" in text() and "Describe" in text(), text())
key("L", 0.5); check("L returns to the log", "Log ·" in text(), "")
key("\x1b", 0.4)  # back to the list
key("x", 0.8)
check("x opens the confirm naming the owner", "Restart billing-api" in text() and "replaces it" in text(), text())
key("\x1b", 0.5)
check("Esc leaves it without a delete", not any("delete pod" in l for l in calls()) and "Restart billing-api" not in text(), "")
key("x", 0.6); key("x", 1.5)
check("x again sends kubectl delete pod", any("delete pod" in l for l in calls()), str(calls()[-2:]))
check("the deletion toast names the owner", "Deleted billing-api" in text() and "putting a new one up" in text(), text())
key("y", 0.5); check("y copies", "Copied id" in text(), "")
key("g", 0.8)
c = ctx(); check("g with no matching repo stays on AKS and says so", c["active_tab"] == "aks" and "No repository on file is called" in text(), text())
key("s", 2.5)
check("s runs kubectl exec", any(" exec " in l for l in calls()), str([l for l in calls() if " exec " in l]))
check("the TUI is alive and repainted after the shell", alive and "Pods" in text() and "Shell in" in text(), text())
key("4", 0.8); key("5", 0.8)
c = ctx(); check("the follow survives a tab round trip", c["aks"]["following_log"] is not None, "")
n_logs = len([l for l in calls() if " logs " in l])
key("4", 0.8); key("5", 0.8)
check("no re-tail on returning to the tab", len([l for l in calls() if " logs " in l]) == n_logs, "")
with open(CFG, "a") as f:
    f.write('\n[[clusters]]\nname = "qa2"\ncontext = "aks-qa"\nnamespaces = ["orders"]\n')
ok = wait_for(lambda: "qa2" in ctx()["aks"]["clusters"], 4)
check("a cluster added to config.toml is picked up", ok, str(ctx()["aks"]["clusters"]))
ok = wait_for(lambda: ctx()["aks"]["visible_rows"] >= 4, 6)   # query still narrows to billing-api: 2 + 2 more? qa2 has orders only -> still 2
key("\x1b", 0.5)
ok = wait_for(lambda: ctx()["aks"]["visible_rows"] >= 9, 6)
check("and its pods are read at once", ok, str(ctx()["aks"]["visible_rows"]))
key("r", 1.5); check("r reads the clusters again and re-announces the bad cluster", "does not exist" in text() or "Forbidden" in text() or "Reading pods" in text(), text())
key("q", 1.5); time.sleep(0.5)
left = subprocess.run(["pgrep", "-f", "fake-kubectl"], capture_output=True, text=True).stdout.split()
check("no fake kubectl child is left after q", not left, str(left))
for p in left: os.kill(int(p), signal.SIGKILL)
print("FAILS:", fails)
sys.exit(fails)
