"""One smoke test for the launcher, against a live daemon.

The launcher is a client: it asks the daemon, over a named pipe, and
prints what the daemon answered. The daemon is started here, on a pipe of
its own, so the test does not talk to the developer's own daemon.

The requests are built here and not in the shell, because a path with a
backslash in it is exactly the kind of thing a shell will helpfully
reinterpret.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import time

root = tempfile.mkdtemp(prefix="rgxy-smoke-")
with open(os.path.join(root, "notas.txt"), "w", encoding="utf-8") as f:
    f.write("linha um\nTesla na segunda\nlinha tres")
with open(os.path.join(root, "outro.txt"), "w", encoding="utf-8") as f:
    f.write("nada aqui")
with open(os.path.join(root, "relatorio.txt"), "w", encoding="utf-8") as f:
    f.write("Tesla outra vez")

pipe = "rgy-rgxy-smoke-%d" % os.getpid()

daemon = sys.argv[1] if len(sys.argv) > 1 else "target/debug/searchd.exe"
launcher = sys.argv[2] if len(sys.argv) > 2 else "target/debug/rgxy.exe"

env = dict(os.environ)
env["RGY_INDEX_DIR"] = os.path.join(root, "idx")

server = subprocess.Popen(
    [daemon, "--root", root, "--listen", "pipe:" + pipe],
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
    env=env,
)
try:
    # The daemon binds the pipe and walks the tree before it answers.
    time.sleep(2.0)

    def ask(query, extra=()):
        proc = subprocess.run(
            [launcher, "--endpoint", "pipe:" + pipe, "--query", query, *extra],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
        )
        return proc

    for query in ["notas", "grep:Tesla", "notas grep:Tesla", "grep:naoexiste"]:
        print("--- query: %s" % query)
        answer = ask(query)
        print(answer.stdout.decode("utf-8"), end="")
        print("exit=%d" % answer.returncode)
        if answer.stderr:
            print("stderr:", answer.stderr.decode("utf-8"))
finally:
    server.terminate()
    try:
        server.wait(timeout=10)
    except subprocess.TimeoutExpired:
        server.kill()
    # The folder is the test's, and the test takes it with it: a test that
    # leaves a folder behind is a test that fills the temporary folder.
    shutil.rmtree(root, ignore_errors=True)
