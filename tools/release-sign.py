#!/usr/bin/env python3
"""Sign and verify SHA256SUMS of a release with Ed25519.

The signature is detached. FILE.sig holds the base64 of the 64 raw signature bytes on one line.
It covers the exact bytes of FILE. Front ends check it against the public key in release-signing.pub.

The private key is the repository secret NXKIT_SIGNING_KEY: the base64 of the 32 raw bytes of the Ed25519 seed.
The public key is the base64 of 32 raw bytes. It is committed in release-signing.pub.
That file has comment lines that start with `#`, then one key line.

Commands (Python 3, run with `python3 -I`, needs `pip install -r tools/requirements-release.txt`):
  keygen [--pub FILE]       print the private key, then the public key, to stdout. Write nothing, except the public key to FILE.
  sign FILE                 read NXKIT_SIGNING_KEY from the environment and write FILE.sig
  verify FILE --pub FILE    check FILE.sig against the public key in the file
  check-key --pub FILE      fail when NXKIT_SIGNING_KEY is missing, when it is not the key of the file, or when the file has no key
"""
import argparse
import base64
import os
import sys
from pathlib import Path

KEY_ENV = "NXKIT_SIGNING_KEY"


class Fail(Exception):
    pass


def crypto():
    try:
        from cryptography.exceptions import InvalidSignature
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric import ed25519
    except ImportError:
        raise Fail("the Python package `cryptography` is missing: python3 -m pip install -r tools/requirements-release.txt")
    return ed25519, serialization, InvalidSignature


def raw(b64, size, what):
    try:
        data = base64.b64decode(b64.strip(), validate=True)
    except Exception:
        raise Fail(f"{what} is not base64")
    if len(data) != size:
        raise Fail(f"{what} must be the base64 of {size} raw bytes (it is {len(data)} bytes)")
    return data


def public_b64(private):
    _, ser, _ = crypto()
    return base64.b64encode(private.public_key().public_bytes(ser.Encoding.Raw, ser.PublicFormat.Raw)).decode()


def env_key():
    secret = os.environ.get(KEY_ENV, "").strip()
    if not secret:
        raise Fail(f"{KEY_ENV} is not set: a release must be signed")
    ed, _, _ = crypto()
    return ed.Ed25519PrivateKey.from_private_bytes(raw(secret, 32, KEY_ENV))


def read_pub(path):
    """The first line of the file that is not blank and not a comment."""
    path = Path(path)
    if not path.is_file():
        raise Fail(f"{path} is missing")
    for line in path.read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            raw(line, 32, str(path))
            return line
    raise Fail(f"{path} has no public key yet: the maintainer must run `keygen` and commit the public key")


def cmd_keygen(args):
    ed, ser, _ = crypto()
    key = ed.Ed25519PrivateKey.generate()
    seed = key.private_bytes(ser.Encoding.Raw, ser.PrivateFormat.Raw, ser.NoEncryption())
    pub = public_b64(key)
    print(f"private (secret {KEY_ENV}): {base64.b64encode(seed).decode()}")
    print(f"public (release-signing.pub): {pub}")
    if args.pub:
        Path(args.pub).write_text(
            "# The public half of the key that signs SHA256SUMS of each release (Ed25519, base64 of the 32 raw bytes), one line.\n"
            f"# The private half is the repository secret {KEY_ENV}.\n{pub}\n"
        )


def cmd_sign(args):
    key = env_key()
    data = Path(args.file).read_bytes()
    Path(str(args.file) + ".sig").write_text(base64.b64encode(key.sign(data)).decode() + "\n")


def cmd_verify(args):
    ed, _, invalid = crypto()
    pub = ed.Ed25519PublicKey.from_public_bytes(raw(read_pub(args.pub), 32, str(args.pub)))
    sig_path = Path(str(args.file) + ".sig")
    if not sig_path.is_file():
        raise Fail(f"{sig_path} is missing")
    sig = raw(sig_path.read_text(), 64, str(sig_path))
    try:
        pub.verify(sig, Path(args.file).read_bytes())
    except invalid:
        raise Fail(f"the signature in {sig_path} does not match {args.file} and the key in {args.pub}")
    print(f"ok: {sig_path} is a valid signature of {args.file}")


def cmd_check_key(args):
    have = public_b64(env_key())
    want = read_pub(args.pub)
    if have != want:
        raise Fail(f"{KEY_ENV} does not belong to the public key in {args.pub}: front ends would reject the release")
    print(f"ok: {KEY_ENV} matches {args.pub}")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("keygen")
    s.add_argument("--pub")
    s.set_defaults(fn=cmd_keygen)
    s = sub.add_parser("sign")
    s.add_argument("file")
    s.set_defaults(fn=cmd_sign)
    s = sub.add_parser("verify")
    s.add_argument("file")
    s.add_argument("--pub", required=True)
    s.set_defaults(fn=cmd_verify)
    s = sub.add_parser("check-key")
    s.add_argument("--pub", required=True)
    s.set_defaults(fn=cmd_check_key)
    args = p.parse_args()
    try:
        args.fn(args)
    except Fail as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
