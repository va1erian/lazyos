"""Throwaway certificates for the TLS harness (docs/tls-plan.md §8).

A test CA (`ca.pem`, appended to the image's bundle through
`LAZYOS_TLS_TEST_CA`) signs the leaves the good servers present; each negative
server gets a leaf a correct client must refuse: expired, not yet valid, for
another name, self-signed, or signed by a second CA the guest has never seen.
Everything is generated at run time with the `cryptography` package and lives
only in the harness's output directory: no key is ever committed.

    python tools/net/tlscerts.py OUTDIR      # write them, print the files
"""

from __future__ import annotations

import datetime
import sys
from dataclasses import dataclass
from pathlib import Path

try:
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec, rsa
    from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID
except ImportError:  # reported by `require()`, so importing this module never fails
    x509 = None

#: The name the good servers answer to; the image's test hosts file maps it
#: (and its subdomains below) to the host as seen from the guest.
NAME = "tls.test"
RSA_NAME = "rsa.tls.test"
OTHER_NAME = "other.test"
DAY = datetime.timedelta(days=1)


def require() -> None:
    """Stop with an instruction when `cryptography` is missing."""
    if x509 is None:
        sys.exit("the TLS harness needs the `cryptography` package: pip install cryptography")


@dataclass
class Leaf:
    """One server identity: PEM files for Python's `ssl`."""
    cert: Path
    key: Path


def _name(common: str) -> "x509.Name":
    return x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, common),
                      x509.NameAttribute(NameOID.ORGANIZATION_NAME, "LazyOS TLS harness")])


def _write(path: Path, cert, key) -> Leaf:
    cert_path, key_path = path.with_suffix(".pem"), path.with_suffix(".key")
    cert_path.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    key_path.write_bytes(key.private_bytes(serialization.Encoding.PEM,
                                           serialization.PrivateFormat.PKCS8,
                                           serialization.NoEncryption()))
    return Leaf(cert_path, key_path)


def _ca(common: str, now: datetime.datetime):
    key = ec.generate_private_key(ec.SECP256R1())
    cert = (x509.CertificateBuilder()
            .subject_name(_name(common)).issuer_name(_name(common))
            .public_key(key.public_key()).serial_number(x509.random_serial_number())
            .not_valid_before(now - DAY).not_valid_after(now + 30 * DAY)
            .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
            .add_extension(x509.KeyUsage(digital_signature=False, content_commitment=False,
                                         key_encipherment=False, data_encipherment=False,
                                         key_agreement=False, key_cert_sign=True, crl_sign=True,
                                         encipher_only=False, decipher_only=False), critical=True)
            .add_extension(x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False)
            .sign(key, hashes.SHA256()))
    return cert, key


def _leaf(names: list[str], issuer, issuer_key, before, after, rsa_key: bool = False):
    key = (rsa.generate_private_key(public_exponent=65537, key_size=2048) if rsa_key
           else ec.generate_private_key(ec.SECP256R1()))
    builder = (x509.CertificateBuilder()
               .subject_name(_name(names[0]))
               .issuer_name(issuer.subject if issuer is not None else _name(names[0]))
               .public_key(key.public_key()).serial_number(x509.random_serial_number())
               .not_valid_before(before).not_valid_after(after)
               .add_extension(x509.SubjectAlternativeName([x509.DNSName(n) for n in names]),
                              critical=False)
               .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
               .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]),
                              critical=False))
    if issuer is not None:
        builder = builder.add_extension(
            x509.AuthorityKeyIdentifier.from_issuer_public_key(issuer.public_key()), critical=False)
    return builder.sign(issuer_key if issuer is not None else key, hashes.SHA256()), key


def generate(out_dir: Path) -> dict[str, Path | Leaf]:
    """Write the CA and every leaf under `out_dir`; return them by role."""
    require()
    out_dir.mkdir(parents=True, exist_ok=True)
    now = datetime.datetime.now(datetime.timezone.utc)
    ca, ca_key = _ca("LazyOS TLS harness test CA", now)
    stranger, stranger_key = _ca("A CA LazyOS has never seen", now)
    files: dict[str, Path | Leaf] = {}
    files["ca"] = _write(out_dir / "ca", ca, ca_key).cert
    valid = (now - DAY, now + 20 * DAY)
    roles = {
        "good": ([NAME], ca, ca_key, *valid, False),
        "rsa": ([RSA_NAME], ca, ca_key, *valid, True),
        "expired": ([NAME], ca, ca_key, now - 40 * DAY, now - 10 * DAY, False),
        "notyet": ([NAME], ca, ca_key, now + 10 * DAY, now + 40 * DAY, False),
        "wrongname": ([OTHER_NAME], ca, ca_key, *valid, False),
        "selfsigned": ([NAME], None, None, *valid, False),
        "unknownca": ([NAME], stranger, stranger_key, *valid, False),
    }
    for role, (names, issuer, issuer_key, before, after, use_rsa) in roles.items():
        cert, key = _leaf(names, issuer, issuer_key, before, after, use_rsa)
        files[role] = _write(out_dir / role, cert, key)
    return files


ROLES = ("good", "rsa", "expired", "notyet", "wrongname", "selfsigned", "unknownca")


def load(out_dir: Path) -> dict[str, Path | Leaf] | None:
    """The files a previous `generate` wrote, or None if any is missing (an
    image built with that CA must be run against the same leaves)."""
    files: dict[str, Path | Leaf] = {"ca": out_dir / "ca.pem"}
    files.update({role: Leaf(out_dir / f"{role}.pem", out_dir / f"{role}.key") for role in ROLES})
    paths = [files["ca"]] + [p for role in ROLES for p in (files[role].cert, files[role].key)]
    return files if all(p.is_file() for p in paths) else None


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    for role, item in generate(Path(sys.argv[1])).items():
        print(role, item)
