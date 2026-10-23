#!/usr/bin/env python3
"""Make the updater's test fixtures: a throwaway minisign key and signed
SHA256SUMS files for a fake release, so the tests need neither the
network nor the owner's key. Run from this folder:

    python3 make-fixtures.py

The key here is for tests only. Its seed is fixed so the output is the
same on every run; nothing it signs is ever trusted outside the tests.
Needs the `cryptography` package (Ed25519) and hashlib's BLAKE2b, which
is what minisign hashes a file with before signing it (the "ED", or
prehashed, signature that current minisign makes by default).
"""
import base64
import hashlib

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

VERSION = "2.0.0"
# The packages' contents must match `fake_package` in update.rs's tests.
PACKAGES = [
    f"Nimbus-{VERSION}-macos.zip",
    f"Nimbus-{VERSION}-x86_64.AppImage",
    f"nimbus-{VERSION}-linux-x86_64.tar.gz",
    f"nimbus-{VERSION}-windows-x86_64-setup.exe",
]


def fake_package(name):
    return f"fake package {name}\n".encode()


def key(seed_byte, key_id):
    sk = Ed25519PrivateKey.from_private_bytes(bytes([seed_byte]) * 32)
    pk = sk.public_key().public_bytes_raw()
    pub = "untrusted comment: minisign public key (tests only)\n"
    pub += base64.b64encode(b"Ed" + key_id + pk).decode() + "\n"
    return sk, key_id, pub


def sign(sk, key_id, data, comment):
    digest = hashlib.blake2b(data, digest_size=64).digest()
    sig = sk.sign(digest)
    trusted = comment.encode()
    glob = sk.sign(sig + trusted)
    return (
        "untrusted comment: signature from minisign secret key\n"
        + base64.b64encode(b"ED" + key_id + sig).decode()
        + "\n"
        + "trusted comment: "
        + comment
        + "\n"
        + base64.b64encode(glob).decode()
        + "\n"
    )


def main():
    sk, kid, pub = key(7, bytes.fromhex("0102030405060708"))
    other_sk, other_kid, _ = key(9, bytes.fromhex("0807060504030201"))
    sums = "".join(
        f"{hashlib.sha256(fake_package(n)).hexdigest()}  {n}\n" for n in PACKAGES
    ).encode()
    open("test-key.pub", "w").write(pub)
    open("SHA256SUMS", "wb").write(sums)
    open("SHA256SUMS.minisig", "w").write(
        sign(sk, kid, sums, f"Nimbus {VERSION} SHA256SUMS")
    )
    # The same list signed as if it were an older release: a replay.
    open("SHA256SUMS-old-version.minisig", "w").write(
        sign(sk, kid, sums, "Nimbus 1.0.0 SHA256SUMS")
    )
    # Signed by a key the tests do not trust.
    open("SHA256SUMS-other-key.minisig", "w").write(
        sign(other_sk, other_kid, sums, f"Nimbus {VERSION} SHA256SUMS")
    )


main()
