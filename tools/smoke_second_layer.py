"""One smoke test for the daemon's second layer.

The request is built here and not in the shell, because a path with a
backslash in it is exactly the kind of thing a shell will helpfully
reinterpret.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

root = tempfile.mkdtemp(prefix="rgy2nd-")
with open(os.path.join(root, "notas.txt"), "w", encoding="utf-8") as f:
    f.write("linha um\nTesla na segunda\nlinha tres")
with open(os.path.join(root, "outro.txt"), "w", encoding="utf-8") as f:
    f.write("nada aqui")
with open(os.path.join(root, "relatorio.pdf"), "w", encoding="utf-8") as f:
    f.write("%PDF-1.4\nnao sou um pdf de verdade")

scope = [
    os.path.join(root, "notas.txt").replace("\\", "/"),
    os.path.join(root, "outro.txt").replace("\\", "/"),
]

requests = [
    {"op": "search", "q": "notas", "limit": 5},
    {"op": "grep-scoped", "q": "tesla", "paths": scope, "limit": 10},
    {"op": "grep-scoped", "q": "nada", "paths": scope, "limit": 10},
    {"op": "grep-scoped", "q": "tesla", "paths": [], "limit": 10},
]

payload = "".join(json.dumps(r) + "\n" for r in requests)

exe = sys.argv[1] if len(sys.argv) > 1 else "target/debug/searchd.exe"
proc = subprocess.run(
    [exe, "--root", root, "--index", os.path.join(root, "idx")],
    input=payload.encode("utf-8"),
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    timeout=60,
)
print(proc.stdout.decode("utf-8"))
print("exit=%d" % proc.returncode)

# The folder is the test's, and the test takes it with it: a test that
# leaves a folder behind is a test that fills the temporary folder.
shutil.rmtree(root, ignore_errors=True)
