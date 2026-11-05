//! Which hosts could be sent a gold app's version (#1418).
//!
//! A gold unit targeted at a host the scheduler can't give any version of the app never leaves
//! the server: the scheduler logs "App version for assigned WU not found" on every request, and
//! the unit counts against the global safety cap. So onboarding skips such hosts.
//!
//! The scheduler (`get_app_version`, BOINC 22bbebe) sends a version only for a platform the
//! client reports, within the version's client bounds, and whose plan class `app_plan` passes.
//! The gold hook (`scripts/boinc/patch-server-gold-routing.py`) then needs the job's base to be
//! the reviewed binding for the version's platform. The host row keeps what the client last
//! reported: `os_name`, `p_vendor`, and in `misc` the client version and any container runtime
//! (`host_info_json`). Platforms aren't stored, so they're inferred from the OS and CPU vendor,
//! and a host neither identifies is judged without them. Of a plan class, only what the row
//! shows is checked: `<docker/>` and the client version bounds.
use super::{query, Config};
use anyhow::{ensure, Context};
use std::collections::{BTreeMap, BTreeSet};

/// One live version of an app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Version {
    pub platform: String,
    pub plan_class: String,
    pub min_core_version: u32,
    pub max_core_version: u32,
}

/// What `plan_class_spec.xml` asks of a host that its row can show.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct PlanClass {
    pub docker: bool,
    pub min_core_client_version: u32,
    pub max_core_client_version: u32,
}

/// A host as its last scheduler request left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Host {
    pub id: u64,
    pub os_name: String,
    pub p_vendor: String,
    /// `major * 10000 + minor * 100 + release`, as the scheduler compares it.
    pub client: Option<u32>,
    /// Docker or Podman, in an allowed WSL distro on Windows.
    pub container: bool,
}

/// What an app's versions need, and the platforms the job's base is bound for.
#[derive(Debug)]
pub(super) struct Requirements {
    pub versions: Vec<Version>,
    pub plan_classes: BTreeMap<String, PlanClass>,
    /// None when no binding table is configured.
    pub bound: Option<BTreeSet<String>>,
}

const X86_VENDORS: [&str; 3] = ["GenuineIntel", "AuthenticAMD", "HygonGenuine"];

/// The platforms a host's client reports, or None when its OS and CPU vendor don't say.
fn platforms(host: &Host) -> Option<&'static [&'static str]> {
    let x86 = X86_VENDORS.contains(&host.p_vendor.as_str());
    let os = host.os_name.as_str();
    if os.starts_with("Microsoft Windows") && x86 {
        Some(&["windows_x86_64", "windows_intelx86"])
    } else if os == "Darwin" && host.p_vendor == "Apple" {
        Some(&["arm64-apple-darwin", "x86_64-apple-darwin"])
    } else if os == "Darwin" && x86 {
        Some(&["x86_64-apple-darwin"])
    } else if os.starts_with("Linux") && x86 {
        Some(&["x86_64-pc-linux-gnu", "i686-pc-linux-gnu"])
    } else {
        None
    }
}

fn client_within(client: Option<u32>, min: u32, max: u32) -> bool {
    match client {
        Some(v) => v >= min && (max == 0 || v <= max),
        None => min == 0 && max == 0,
    }
}

impl Requirements {
    /// Whether any version could be sent to `host`. A plan class missing from the spec is
    /// one the scheduler refuses.
    pub fn can_run(&self, host: &Host) -> bool {
        let platforms = platforms(host);
        self.versions.iter().any(|v| {
            platforms.is_none_or(|p| p.contains(&v.platform.as_str()))
                && self.bound.as_ref().is_none_or(|b| b.contains(&v.platform))
                && client_within(host.client, v.min_core_version, v.max_core_version)
                && (v.plan_class.is_empty()
                    || self.plan_classes.get(&v.plan_class).is_some_and(|c| {
                        (!c.docker || host.container)
                            && client_within(
                                host.client,
                                c.min_core_client_version,
                                c.max_core_client_version,
                            )
                    }))
        })
    }
}

/// `x.y.z` as BOINC's `core_client_version`.
fn client_version(text: &str) -> Option<u32> {
    let parts: Vec<u32> = text
        .split('.')
        .map(|p| p.parse().ok().filter(|n| *n < 100))
        .collect::<Option<_>>()?;
    match parts[..] {
        [major, minor, release] => Some(major * 10000 + minor * 100 + release),
        _ => None,
    }
}

/// Rows of `id, os_name, p_vendor, client_version, container` from `mysql --batch`, which
/// escapes tabs and newlines inside values.
fn parse_hosts(text: &str) -> anyhow::Result<Vec<Host>> {
    let mut hosts = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        ensure!(f.len() == 5, "invalid host inventory row");
        let id: u64 = f[0].parse().context("invalid host inventory")?;
        ensure!(id > 0 && seen.insert(id), "invalid host inventory");
        hosts.push(Host {
            id,
            os_name: f[1].into(),
            p_vendor: f[2].into(),
            client: client_version(f[3]),
            container: match f[4] {
                "1" => true,
                "0" => false,
                _ => anyhow::bail!("invalid host container field"),
            },
        });
    }
    ensure!(hosts.len() <= 100000, "host inventory exceeds safety bound");
    Ok(hosts)
}

fn parse_versions(text: &str) -> anyhow::Result<Vec<Version>> {
    let name = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
    };
    let mut versions = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        ensure!(
            f.len() == 4 && !f[0].is_empty() && name(f[0]) && name(f[1]),
            "invalid app version row"
        );
        versions.push(Version {
            platform: f[0].into(),
            plan_class: f[1].into(),
            min_core_version: f[2].parse().context("invalid app version row")?,
            max_core_version: f[3].parse().context("invalid app version row")?,
        });
    }
    ensure!(versions.len() <= 1000, "app versions exceed safety bound");
    Ok(versions)
}

/// The plan classes of a `plan_class_spec.xml`. The scheduler uses the first of a name.
fn parse_plan_classes(xml: &str) -> anyhow::Result<BTreeMap<String, PlanClass>> {
    use quick_xml::{events::Event, Reader};
    const FIELDS: [&str; 4] = [
        "name",
        "docker",
        "min_core_client_version",
        "max_core_client_version",
    ];
    let mut reader = Reader::from_str(xml);
    let mut path: Vec<String> = Vec::new();
    let mut classes = BTreeMap::new();
    let mut current: Option<(String, PlanClass)> = None;
    let mut text = String::new();
    let number = |text: &str| {
        text.trim()
            .parse::<u32>()
            .context("invalid plan class number")
    };
    loop {
        let event = reader.read_event()?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(e) | Event::Empty(e) => {
                let name = e.name().as_ref().to_owned();
                if path.is_empty() {
                    ensure!(name == "plan_classes", "expected plan_classes root");
                }
                path.push(name);
                text.clear();
                if path == ["plan_classes", "plan_class"] {
                    current = Some(Default::default());
                }
                if empty {
                    // `<docker/>` is BOINC's archaic true.
                    if let (Some((_, class)), 3) = (&mut current, path.len()) {
                        if path[2] == "docker" {
                            class.docker = true;
                        }
                    }
                    path.pop();
                }
            }
            Event::Text(e) => text.push_str(&e),
            Event::CData(e) => text.push_str(&e),
            Event::GeneralRef(_) if path.len() == 3 && FIELDS.contains(&path[2].as_str()) => {
                anyhow::bail!("plan class fields must be literal")
            }
            Event::End(_) => {
                if let (Some((name, class)), 3) = (&mut current, path.len()) {
                    match path[2].as_str() {
                        "name" => *name = text.trim().to_owned(),
                        "docker" => class.docker = number(&text)? != 0,
                        "min_core_client_version" => class.min_core_client_version = number(&text)?,
                        "max_core_client_version" => class.max_core_client_version = number(&text)?,
                        _ => {}
                    }
                }
                if path.len() == 2 {
                    if let Some((name, class)) = current.take() {
                        classes.entry(name).or_insert(class);
                    }
                }
                path.pop();
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!(path.is_empty(), "unterminated plan class spec");
    Ok(classes)
}

/// The platforms whose reviewed base for `image` is `base`, from `gold-engine-platforms.json`.
fn bound_platforms(json: &[u8], image: &str, base: &str) -> anyhow::Result<BTreeSet<String>> {
    let table: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_slice(json).context("invalid gold engine platform table")?;
    Ok(table
        .get(image)
        .into_iter()
        .flatten()
        .filter(|(_, b)| *b == base)
        .map(|(platform, _)| platform.clone())
        .collect())
}

/// Every host BOINC knows, in id order, with what its last request reported. The text fields
/// are cut to what `platforms` reads, so 100000 hosts fit `query`'s output limit.
pub(super) async fn inventory(c: &Config) -> anyhow::Result<Vec<Host>> {
    parse_hosts(
        &query(
            c,
            "SELECT id, LEFT(os_name, 20), LEFT(p_vendor, 12), \
             IF(JSON_VALID(misc), LEFT(JSON_UNQUOTE(JSON_EXTRACT(misc, '$.client_version')), 8), ''), \
             IF(JSON_VALID(misc), JSON_CONTAINS_PATH(misc, 'one', '$.docker'), 0) \
             FROM host WHERE id>0 ORDER BY id LIMIT 100001",
        )
        .await?,
    )
}

/// What a host needs to be sent one of `app`'s live versions for this campaign's job.
pub(super) async fn requirements(c: &Config, app: &str) -> anyhow::Result<Requirements> {
    ensure!(
        crate::gold::submit::is_gold_app(app),
        "{app} is not a gold app"
    );
    let versions = parse_versions(
        &query(
            c,
            &format!(
                "SELECT p.name, av.plan_class, av.min_core_version, av.max_core_version \
                 FROM app_version av JOIN app a ON a.id=av.appid JOIN platform p ON p.id=av.platformid \
                 WHERE a.name='{app}' AND a.deprecated=0 AND av.deprecated=0 AND p.deprecated=0 \
                 ORDER BY av.id LIMIT 1001"
            ),
        )
        .await?,
    )?;
    let plan_classes = if versions.iter().any(|v| !v.plan_class.is_empty()) {
        let path = c.project.join("plan_class_spec.xml");
        let bytes =
            crate::read(&path, 1 << 20).with_context(|| format!("reading {}", path.display()))?;
        parse_plan_classes(std::str::from_utf8(&bytes).context("plan class spec is not UTF-8")?)?
    } else {
        BTreeMap::new()
    };
    let bound = match &c.engine_platforms {
        Some(path) => Some(bound_platforms(
            &crate::read(path, 1 << 20)?,
            &c.engine.image,
            &c.engine.base,
        )?),
        None => None,
    };
    Ok(Requirements {
        versions,
        plan_classes,
        bound,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: u64, os: &str, vendor: &str, client: &str, container: bool) -> Host {
        Host {
            id,
            os_name: os.into(),
            p_vendor: vendor.into(),
            client: client_version(client),
            container,
        }
    }

    fn version(platform: &str, plan_class: &str) -> Version {
        Version {
            platform: platform.into(),
            plan_class: plan_class.into(),
            min_core_version: 0,
            max_core_version: 0,
        }
    }

    const SPEC: &str = r#"<?xml version="1.0"?>
<!-- comment -->
<plan_classes>
    <plan_class>
        <name>docker</name>
        <docker/>
        <min_core_client_version>80211</min_core_client_version>
    </plan_class>
    <plan_class>
        <name>docker</name>
    </plan_class>
    <plan_class>
        <name>mt</name>
        <docker>0</docker>
        <max_core_client_version>80299</max_core_client_version>
    </plan_class>
</plan_classes>
"#;

    /// `usnm_gold` as published on 5 November 2026: the container class on x86-64 Linux and
    /// Apple silicon.
    fn container_gold(bound: Option<&[&str]>) -> Requirements {
        Requirements {
            versions: vec![
                version("x86_64-pc-linux-gnu", "docker"),
                version("arm64-apple-darwin", "docker"),
            ],
            plan_classes: parse_plan_classes(SPEC).unwrap(),
            bound: bound.map(|b| b.iter().map(|p| p.to_string()).collect()),
        }
    }

    #[test]
    fn hosts_without_a_container_runtime_cant_run_container_gold() {
        let gold = container_gold(None);
        // Hosts 5 and 13 in #1418: no container runtime reported.
        assert!(!gold.can_run(&host(
            5,
            "Microsoft Windows 11",
            "AuthenticAMD",
            "8.2.11",
            false
        )));
        assert!(!gold.can_run(&host(13, "Linux Ubuntu", "AuthenticAMD", "8.2.11", false)));
        assert!(gold.can_run(&host(
            9,
            "Linux Debian GNU/Linux",
            "GenuineIntel",
            "8.2.13",
            true
        )));
        assert!(gold.can_run(&host(1, "Darwin", "Apple", "8.2.11", true)));
        // A container but a client under the class's minimum, or no request on record.
        assert!(!gold.can_run(&host(13, "Linux Ubuntu", "AuthenticAMD", "8.2.9", true)));
        assert!(!gold.can_run(&host(4, "Darwin", "Apple", "", false)));
        // Windows with Docker in WSL: no Windows version.
        assert!(!gold.can_run(&host(
            5,
            "Microsoft Windows 11",
            "AuthenticAMD",
            "8.2.11",
            true
        )));
        // An Intel Mac and Linux on x86-64 are different platforms.
        assert!(!gold.can_run(&host(6, "Darwin", "GenuineIntel", "8.2.11", true)));
        // A host whose platform isn't known is judged on the plan class alone.
        assert!(gold.can_run(&host(7, "Linux Debian", "ARM", "8.2.13", true)));
        assert!(!gold.can_run(&host(7, "Linux Debian", "ARM", "8.2.13", false)));
    }

    #[test]
    fn the_job_base_must_be_bound_for_the_hosts_platform() {
        // The x86-64 base of worker 247c is bound for x86-64 Linux and Intel Macs only.
        let gold = container_gold(Some(&["x86_64-pc-linux-gnu", "x86_64-apple-darwin"]));
        assert!(gold.can_run(&host(9, "Linux Debian", "GenuineIntel", "8.2.13", true)));
        assert!(!gold.can_run(&host(1, "Darwin", "Apple", "8.2.11", true)));
        assert!(!container_gold(Some(&[])).can_run(&host(
            9,
            "Linux",
            "GenuineIntel",
            "8.2.13",
            true
        )));
    }

    #[test]
    fn native_gold_needs_no_container_and_unknown_plan_classes_never_run() {
        let native = Requirements {
            versions: vec![version("x86_64-pc-linux-gnu", "")],
            plan_classes: BTreeMap::new(),
            bound: None,
        };
        assert!(native.can_run(&host(13, "Linux Ubuntu", "AuthenticAMD", "8.2.9", false)));
        assert!(!native.can_run(&host(
            5,
            "Microsoft Windows 11",
            "AuthenticAMD",
            "8.2.11",
            false
        )));
        let none = Requirements {
            versions: vec![],
            plan_classes: BTreeMap::new(),
            bound: None,
        };
        assert!(!none.can_run(&host(9, "Linux", "GenuineIntel", "8.2.13", true)));
        let unknown = Requirements {
            versions: vec![version("x86_64-pc-linux-gnu", "docker_gpu")],
            plan_classes: parse_plan_classes(SPEC).unwrap(),
            bound: None,
        };
        assert!(!unknown.can_run(&host(9, "Linux", "GenuineIntel", "8.2.13", true)));
        // The version's own client bounds.
        let mut bounded = version("x86_64-pc-linux-gnu", "");
        bounded.min_core_version = 80213;
        let bounded = Requirements {
            versions: vec![bounded],
            plan_classes: BTreeMap::new(),
            bound: None,
        };
        assert!(bounded.can_run(&host(9, "Linux", "GenuineIntel", "8.2.13", false)));
        assert!(!bounded.can_run(&host(13, "Linux", "AuthenticAMD", "8.2.9", false)));
    }

    #[test]
    fn plan_class_spec_keeps_the_first_of_a_name_and_boincs_booleans() {
        let classes = parse_plan_classes(SPEC).unwrap();
        assert_eq!(
            classes["docker"],
            PlanClass {
                docker: true,
                min_core_client_version: 80211,
                max_core_client_version: 0
            }
        );
        assert_eq!(
            classes["mt"],
            PlanClass {
                docker: false,
                min_core_client_version: 0,
                max_core_client_version: 80299
            }
        );
        let repo = include_str!("../../../../contrib-compute/boinc/plan_class_spec.xml");
        assert!(parse_plan_classes(repo).unwrap()["docker"].docker);
        for bad in [
            "<plan_class><name>x</name></plan_class>",
            "<plan_classes><plan_class><name>x</name><docker>yes</docker></plan_class></plan_classes>",
            "<plan_classes><plan_class><name>x</name>",
            "<plan_classes><plan_class><name>&amp;</name></plan_class></plan_classes>",
        ] {
            assert!(parse_plan_classes(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn inventory_and_version_rows_fail_closed() {
        let hosts = parse_hosts(
            "5\tMicrosoft Windows 11\tAuthenticAMD\t8.2.11\t0\n9\tLinux Debian\tGenuineIntel\t8.2.13\t1\n4\tDarwin\tApple\t\t0\n",
        )
        .unwrap();
        assert_eq!(
            hosts[0],
            host(5, "Microsoft Windows 11", "AuthenticAMD", "8.2.11", false)
        );
        assert_eq!(hosts[1].client, Some(80213));
        assert!(hosts[1].container);
        assert_eq!(hosts[2].client, None);
        for bad in [
            "0\tLinux\tGenuineIntel\t8.2.13\t1",
            "1\tLinux\tGenuineIntel\t8.2.13",
            "1\tLinux\tGenuineIntel\t8.2.13\tNULL",
            "1\tLinux\tGenuineIntel\t8.2.13\t1\n1\tLinux\tGenuineIntel\t8.2.13\t1",
        ] {
            assert!(parse_hosts(bad).is_err(), "{bad}");
        }
        for (text, version) in [
            ("8.2.13", Some(80213)),
            ("8.2", None),
            ("8.200.1", None),
            ("NULL", None),
        ] {
            assert_eq!(client_version(text), version, "{text}");
        }
        assert_eq!(
            parse_versions("x86_64-pc-linux-gnu\tdocker\t0\t0\nx86_64-pc-linux-gnu\t\t80211\t0\n")
                .unwrap(),
            vec![version("x86_64-pc-linux-gnu", "docker"), {
                let mut v = version("x86_64-pc-linux-gnu", "");
                v.min_core_version = 80211;
                v
            }]
        );
        for bad in [
            "\tdocker\t0\t0",
            "x86_64-pc-linux-gnu\tdocker\t0",
            "x86_64-pc-linux-gnu\tdo cker\t0\t0",
            "p\tdocker\t-1\t0",
        ] {
            assert!(parse_versions(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn bound_platforms_are_those_with_the_jobs_base() {
        let table = br#"{"sha256:i": {"x86_64-pc-linux-gnu": "sha256:x", "arm64-apple-darwin": "sha256:a"}}"#;
        assert_eq!(
            bound_platforms(table, "sha256:i", "sha256:x").unwrap(),
            ["x86_64-pc-linux-gnu".to_string()].into()
        );
        assert!(bound_platforms(table, "sha256:other", "sha256:x")
            .unwrap()
            .is_empty());
        assert!(bound_platforms(b"[]", "sha256:i", "sha256:x").is_err());
    }
}
