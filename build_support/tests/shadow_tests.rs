//! `/system/etc/shadow` (issue #447): the build ships Argon2id verifiers of
//! the default passwords, which `keyd` checks with the same cost, and no
//! plaintext anywhere.

use lazyos_crypto::kdf;

use crate::os_layout::file_mode;
use crate::shadow::{build, PASSWORDS};

const PASSWD: &[u8] = include_bytes!("../passwd");

#[test]
fn every_account_gets_a_verifier_of_its_password() {
    let bytes = build(PASSWD);
    let rows = passwd::shadow::parse(&bytes).unwrap();
    let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, ["admin", "user"]);
    let text = String::from_utf8(bytes.clone()).unwrap();
    for line in PASSWORDS
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
    {
        let (name, password) = line.split_once(':').unwrap();
        let row = rows.iter().find(|row| row.name == name).unwrap();
        let params = kdf::Params::INTERACTIVE;
        assert_eq!(
            (row.cost.m_kib, row.cost.t, row.cost.p),
            (params.m_cost_kib, params.t_cost, params.p_cost)
        );
        let mut again = [0u8; 32];
        kdf::argon2id(password.as_bytes(), &row.salt, params, &mut again).unwrap();
        assert_eq!(again, row.verifier, "{name}");
        // Neither file carries the password.
        assert!(!text.contains(&format!(":{password}")), "{name}");
        assert!(!String::from_utf8_lossy(PASSWD).contains(password), "{name}");
    }
}

#[test]
fn the_build_is_reproducible_and_root_only() {
    assert_eq!(build(PASSWD), build(PASSWD));
    assert_eq!(file_mode(fhs::etc::SHADOW), 0o600);
    assert_eq!(file_mode(fhs::etc::PASSWD), 0o644);
}
