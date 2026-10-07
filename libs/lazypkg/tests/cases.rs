//! The manifest and version cases shared with the Python builder
//! (`tests/cases/manifest.toml`; `tools/pkg/test_build.py` runs the same file),
//! so the two validators cannot drift apart.

use serde::Deserialize;

const CASES: &str = include_str!("cases/manifest.toml");

#[derive(Deserialize)]
struct Cases {
    manifest: Vec<ManifestCase>,
    versions: Versions,
}

#[derive(Deserialize)]
struct ManifestCase {
    name: String,
    #[serde(default)]
    app: String,
    #[serde(default)]
    entry: String,
    #[serde(default)]
    permissions: String,
    #[serde(default)]
    version: Option<String>,
    valid: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    autostart: Option<bool>,
    #[serde(default)]
    resident: Option<bool>,
    #[serde(default)]
    develop: Option<bool>,
}

#[derive(Deserialize)]
struct Versions {
    valid: Vec<String>,
    invalid: Vec<InvalidVersion>,
    ascending: Vec<Vec<String>>,
    equal: Vec<[String; 2]>,
}

#[derive(Deserialize)]
struct InvalidVersion {
    text: String,
    reason: String,
}

fn cases() -> Cases {
    toml::from_str(CASES).expect("tests/cases/manifest.toml parses")
}

/// The template documented at the top of the cases file.
fn manifest_text(case: &ManifestCase) -> String {
    format!(
        "[app]\nname = \"Demo\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"Tester\"\n\
         version = \"{}\"\n{}\n[entry]\nbinary = \"bin/app.elf\"\n{}\n[permissions]\n{}\n",
        case.version.as_deref().unwrap_or("1.0.0"),
        case.app,
        case.entry,
        case.permissions
    )
}

fn version(text: &str) -> lazypkg::Version {
    lazypkg::Version::parse(text).unwrap_or_else(|error| panic!("{text:?}: {error}"))
}

#[test]
fn manifest_cases() {
    let cases = cases();
    assert!(cases.manifest.len() >= 30, "the cases file lost cases");
    for case in &cases.manifest {
        let result = lazypkg::parse_manifest(&manifest_text(case));
        match (case.valid, result) {
            (true, Ok(manifest)) => {
                if let Some(category) = &case.category {
                    assert_eq!(manifest.app.category().as_str(), category, "{}", case.name);
                }
                if let Some(autostart) = case.autostart {
                    assert_eq!(manifest.entry.autostart, autostart, "{}", case.name);
                }
                if let Some(resident) = case.resident {
                    assert_eq!(manifest.entry.resident, resident, "{}", case.name);
                }
                if let Some(develop) = case.develop {
                    assert_eq!(manifest.permissions.develop, develop, "{}", case.name);
                }
            }
            (true, Err(error)) => panic!("{}: expected valid, got {error}", case.name),
            (false, Ok(_)) => panic!("{}: expected an error", case.name),
            (false, Err(error)) => {
                let wanted = case
                    .error
                    .as_deref()
                    .expect("an invalid case names its error");
                assert!(
                    error
                        .problems()
                        .iter()
                        .any(|p| p.message().contains(wanted)),
                    "{}: no problem mentions {wanted:?}: {error}",
                    case.name
                );
            }
        }
    }
}

#[test]
fn version_cases() {
    let versions = cases().versions;
    for text in &versions.valid {
        assert_eq!(version(text).as_str(), text);
    }
    for case in &versions.invalid {
        match lazypkg::Version::parse(&case.text) {
            Ok(_) => panic!("{:?} should be invalid", case.text),
            Err(error) => assert_eq!(error.to_string(), case.reason, "{:?}", case.text),
        }
    }
    for chain in &versions.ascending {
        for (index, lower) in chain.iter().enumerate() {
            for higher in &chain[index + 1..] {
                assert!(version(lower) < version(higher), "{lower} < {higher}");
                assert!(version(higher) > version(lower), "{higher} > {lower}");
                assert_ne!(version(lower), version(higher));
            }
        }
    }
    for [left, right] in &versions.equal {
        assert_eq!(version(left), version(right), "{left} == {right}");
        assert_eq!(
            version(left).cmp(&version(right)),
            std::cmp::Ordering::Equal
        );
    }
}
