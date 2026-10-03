#!/usr/bin/env python3
"""Regenerate tlsfix's test PKI (``testdata/``): a P-256 CA and a leaf for
``tlsfix.test`` with its PKCS#8 key, valid 2000-01-01 .. 2099-12-31 so the
fixture works whatever the guest clock says (LazyOS falls back to 2026-01-01).

The key is a test key for an in-memory handshake inside one process; it
protects nothing. Needs ``pip install cryptography``.
"""

import datetime
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

OUT = Path(__file__).resolve().parent / "testdata"
START = datetime.datetime(2000, 1, 1, tzinfo=datetime.timezone.utc)
END = datetime.datetime(2099, 12, 31, 23, 59, 59, tzinfo=datetime.timezone.utc)


def name(cn: str) -> x509.Name:
    return x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, cn)])


def main() -> None:
    OUT.mkdir(exist_ok=True)
    ca_key = ec.generate_private_key(ec.SECP256R1())
    ca = (
        x509.CertificateBuilder()
        .subject_name(name("tlsfix test CA"))
        .issuer_name(name("tlsfix test CA"))
        .public_key(ca_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(START)
        .not_valid_after(END)
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .add_extension(x509.KeyUsage(False, False, False, False, False, True, True, False, False), critical=True)
        .sign(ca_key, hashes.SHA256())
    )
    leaf_key = ec.generate_private_key(ec.SECP256R1())
    leaf = (
        x509.CertificateBuilder()
        .subject_name(name("tlsfix.test"))
        .issuer_name(ca.subject)
        .public_key(leaf_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(START)
        .not_valid_after(END)
        .add_extension(x509.SubjectAlternativeName([x509.DNSName("tlsfix.test")]), critical=False)
        .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), critical=False)
        .sign(ca_key, hashes.SHA256())
    )
    # A second, unrelated CA: the client trusting it must refuse the leaf.
    other_key = ec.generate_private_key(ec.SECP256R1())
    other = (
        x509.CertificateBuilder()
        .subject_name(name("tlsfix unrelated CA"))
        .issuer_name(name("tlsfix unrelated CA"))
        .public_key(other_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(START)
        .not_valid_after(END)
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .sign(other_key, hashes.SHA256())
    )
    der = serialization.Encoding.DER
    (OUT / "ca.der").write_bytes(ca.public_bytes(der))
    (OUT / "other-ca.der").write_bytes(other.public_bytes(der))
    (OUT / "leaf.der").write_bytes(leaf.public_bytes(der))
    (OUT / "leaf.key.pk8").write_bytes(
        leaf_key.private_bytes(der, serialization.PrivateFormat.PKCS8, serialization.NoEncryption())
    )


if __name__ == "__main__":
    main()
