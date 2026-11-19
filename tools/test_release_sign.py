#!/usr/bin/env python3
"""Test of tools/release-sign.py: keygen, sign, verify, a changed file, a wrong key, check-key.
Run: python3 -I tools/test_release_sign.py"""
import os
import subprocess
import sys
import tempfile
from pathlib import Path

TOOL = Path(__file__).resolve().parent / "release-sign.py"


def run(*args, key=None):
    env = {k: v for k, v in os.environ.items() if k != "NXKIT_SIGNING_KEY"}
    if key is not None:
        env["NXKIT_SIGNING_KEY"] = key
    return subprocess.run([sys.executable, "-I", str(TOOL), *args], capture_output=True, text=True, env=env)


def keygen(pub_file):
    r = run("keygen", "--pub", str(pub_file))
    assert r.returncode == 0, r.stderr
    priv = r.stdout.splitlines()[0].split(": ", 1)[1]
    return priv


def main():
    with tempfile.TemporaryDirectory() as d:
        d = Path(d)
        pub, other_pub = d / "a.pub", d / "b.pub"
        priv, other_priv = keygen(pub), keygen(other_pub)
        assert priv != other_priv
        sums = d / "SHA256SUMS"
        sums.write_bytes(b"abc  file.zip\n")

        assert run("check-key", "--pub", str(pub), key=priv).returncode == 0
        assert run("check-key", "--pub", str(pub)).returncode != 0, "missing key must fail"
        assert run("check-key", "--pub", str(pub), key=other_priv).returncode != 0, "wrong key must fail"
        empty = d / "empty.pub"
        empty.write_text("# no key yet\n\n")
        assert run("check-key", "--pub", str(empty), key=priv).returncode != 0, "empty public key must fail"

        assert run("sign", str(sums)).returncode != 0, "sign without a key must fail"
        assert run("sign", str(sums), key=priv).returncode == 0
        sig = (d / "SHA256SUMS.sig").read_text()
        assert sig.endswith("\n") and sig.count("\n") == 1 and len(sig.strip()) == 88, "one base64 line of 64 bytes"
        assert run("verify", str(sums), "--pub", str(pub)).returncode == 0
        assert run("verify", str(sums), "--pub", str(other_pub)).returncode != 0, "wrong public key must fail"
        assert run("verify", str(sums), "--pub", str(empty)).returncode != 0

        sums.write_bytes(b"abd  file.zip\n")
        assert run("verify", str(sums), "--pub", str(pub)).returncode != 0, "changed file must fail"
        sums.write_bytes(b"abc  file.zip\n")
        (d / "SHA256SUMS.sig").unlink()
        assert run("verify", str(sums), "--pub", str(pub)).returncode != 0, "missing signature must fail"

        # keygen writes nothing without --pub
        before = set(d.iterdir())
        assert run("keygen").returncode == 0
        assert set(d.iterdir()) == before
    print("release-sign tests passed")


if __name__ == "__main__":
    main()
