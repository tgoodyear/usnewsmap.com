//! Which hosts could be sent a gold app's version (#1418).
//!
//! A gold unit targeted at a host the scheduler can't give any version of the app never leaves
//! the server: the scheduler logs "App version for assigned WU not found" on every request, and
//! the unit counts against the global safety cap. So onboarding skips such hosts.
//!
//! The scheduler (BOINC 22bbebe) loads only the latest version of each platform and plan class,
//! unless the app sets a `min_version` (`sched_shmem.cpp`), and `get_app_version` sends one of
//! those only for a platform the client reports, within the version's client bounds, and whose
//! plan class `app_plan` passes.
//! The gold hook (`scripts/boinc/gold-engine-routing.h`) then needs the version to package the
//! job's engine and the job's base to be the reviewed binding for the version's platform. The
//! host row keeps what the client last reported: `os_name`, `p_vendor`, and in `misc` the
//! client version, any container runtime and the client config's Docker and WSL refusals
//! (`host_info_json`). Platforms aren't stored, so they're inferred from the OS and CPU vendor,
//! and a host whose OS and CPU vendor give none is judged without them. A host with no `misc`
//! is judged without what it holds. Of a plan class, only what the row shows is checked:
//! `<docker/>` and the client version bounds.
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
    /// `major * 10000 + minor * 100 + release`, as the scheduler compares it. None when the
    /// row has no `misc`.
    pub client: Option<u32>,
    /// `misc.docker`: Docker or Podman, in an allowed WSL distro on Windows. None when the row
    /// has no `misc`.
    pub docker: Option<bool>,
    /// `misc.config`: the client config's `dont_use_docker` and `dont_use_wsl`.
    pub dont_use_docker: bool,
    pub dont_use_wsl: bool,
}

impl Host {
    /// `plan_class_spec.cpp`'s `<docker/>` check. On Windows the row reports Docker only from
    /// an allowed WSL distro. A class's `min_boinc_wsl_distro_version` isn't checked: the row
    /// doesn't keep that version, but the BOINC WSL distro reports its Podman as Docker.
    fn passes_docker(&self) -> bool {
        let windows = self.os_name.contains("Windows");
        !self.dont_use_docker
            && !(windows && self.dont_use_wsl)
            && self.docker != Some(false)
            && !(self.os_name.contains("Darwin") && self.client.is_some_and(|v| v < 80206))
    }
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
    client.is_none_or(|v| v >= min && (max == 0 || v <= max))
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
                        (!c.docker || host.passes_docker())
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

/// Rows of `id, os_name, p_vendor, client_version, docker, dont_use_docker, dont_use_wsl` from
/// `mysql --batch`, which escapes tabs and newlines inside values.
fn parse_hosts(text: &str) -> anyhow::Result<Vec<Host>> {
    let flag = |field: &str| match field {
        "1" => Ok(true),
        "0" => Ok(false),
        _ => anyhow::bail!("invalid host container field"),
    };
    let reported = |field: &str| match field {
        "" => Ok(None),
        _ => flag(field).map(Some),
    };
    let mut hosts = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        ensure!(f.len() == 7, "invalid host inventory row");
        let id: u64 = f[0].parse().context("invalid host inventory")?;
        ensure!(id > 0 && seen.insert(id), "invalid host inventory");
        hosts.push(Host {
            id,
            os_name: f[1].into(),
            p_vendor: f[2].into(),
            client: client_version(f[3]),
            docker: reported(f[4])?,
            dont_use_docker: flag(f[5])?,
            dont_use_wsl: flag(f[6])?,
        });
    }
    ensure!(hosts.len() <= 100000, "host inventory exceeds safety bound");
    Ok(hosts)
}

/// A value as `mysql --batch` wrote it.
fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some(other) => out.extend(['\\', other]),
            None => out.push('\\'),
        }
    }
    out
}

/// A live app version as `requirements` reads it.
#[derive(Debug, PartialEq, Eq)]
struct VersionRow {
    version: Version,
    version_num: u32,
    /// The app's `min_version`.
    app_min_version: u32,
    xml_doc: String,
}

/// Rows of `platform, plan_class, version_num, min_core_version, max_core_version,
/// app.min_version, xml_doc`.
fn parse_versions(text: &str) -> anyhow::Result<Vec<VersionRow>> {
    let name = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
    };
    let number = |s: &str| s.parse::<u32>().context("invalid app version row");
    // The scheduler reads a client bound under 10000 as a release without its minor version.
    let core = |s: &str| number(s).map(|v| if v < 10000 { v * 100 } else { v });
    let mut versions = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        ensure!(
            f.len() == 7 && !f[0].is_empty() && name(f[0]) && name(f[1]),
            "invalid app version row"
        );
        versions.push(VersionRow {
            version: Version {
                platform: f[0].into(),
                plan_class: f[1].into(),
                min_core_version: core(f[3])?,
                max_core_version: core(f[4])?,
            },
            version_num: number(f[2])?,
            app_min_version: number(f[5])?,
            xml_doc: unescape(f[6]),
        });
    }
    ensure!(versions.len() <= 1000, "app versions exceed safety bound");
    Ok(versions)
}

/// The versions the scheduler loads (`sched_shmem.cpp`): the latest of each platform and plan
/// class, or every version from the app's `min_version` on when it has one.
fn loaded(rows: Vec<VersionRow>) -> Vec<VersionRow> {
    let key = |r: &VersionRow| (r.version.platform.clone(), r.version.plan_class.clone());
    let mut latest = BTreeMap::new();
    for r in &rows {
        let num = latest.entry(key(r)).or_insert(r.version_num);
        *num = (*num).max(r.version_num);
    }
    rows.into_iter()
        .filter(|r| match r.app_min_version {
            0 => latest[&key(r)] == r.version_num,
            min => r.version_num >= min,
        })
        .collect()
}

/// The one `<name>` value in `xml`, as the gold hook's `tag` reads it.
fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let mut values = xml.match_indices(&open).filter_map(|(i, _)| {
        let after = &xml[i + open.len()..];
        let end = after.find('<').unwrap_or(after.len());
        after[end..]
            .starts_with(&close)
            .then(|| after[..end].trim())
    });
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

/// Whether a version packages the job's engine, as the gold hook requires: a native build's
/// manifest is named for the image's digest, and a container version's Dockerfile, read from
/// the download directory, is `FROM` the image.
fn packages_engine(
    xml_doc: &str,
    native: bool,
    image: &str,
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> bool {
    let open_name = if native {
        "usnm_native_build.json"
    } else {
        "Dockerfile"
    };
    let mut file = None;
    let mut rest = xml_doc;
    while let Some(start) = rest.find("<file_ref>") {
        let body = &rest[start + "<file_ref>".len()..];
        let Some(end) = body.find("</file_ref>") else {
            break;
        };
        let reference = &body[..end];
        rest = &body[end + "</file_ref>".len()..];
        if !reference.contains("<open_name>") {
            continue;
        }
        match tag(reference, "open_name") {
            Some(name) if name != open_name => continue,
            Some(_) if file.is_none() => match tag(reference, "file_name") {
                Some(name) => file = Some(name),
                None => return false,
            },
            _ => return false,
        }
    }
    let Some(file) = file else {
        return false;
    };
    if native {
        let digest = image.split_once(':').map_or(image, |(_, d)| d);
        return file == format!("usnm_native_build_{digest}.json");
    }
    let expected = format!("FROM ghcr.io/tgoodyear/usnm-boinc-worker@{image}");
    !file.is_empty()
        && file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        && read(file).is_some_and(|payload| {
            payload == expected.as_bytes() || payload == format!("{expected}\n").as_bytes()
        })
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
/// are cut to what `platforms` and `passes_docker` read, so 100000 hosts fit `query`'s output
/// limit.
pub(super) async fn inventory(c: &Config) -> anyhow::Result<Vec<Host>> {
    parse_hosts(
        &query(
            c,
            "SELECT id, LEFT(os_name, 20), LEFT(p_vendor, 12), \
             IF(JSON_VALID(misc), LEFT(JSON_UNQUOTE(JSON_EXTRACT(misc, '$.client_version')), 8), ''), \
             IF(JSON_VALID(misc), COALESCE(JSON_TYPE(JSON_EXTRACT(misc, '$.docker')) = 'OBJECT', 0), ''), \
             IF(JSON_VALID(misc), COALESCE(JSON_CONTAINS(misc, 'true', '$.config.dont_use_docker'), 0), 0), \
             IF(JSON_VALID(misc), COALESCE(JSON_CONTAINS(misc, 'true', '$.config.dont_use_wsl'), 0), 0) \
             FROM host WHERE id>0 ORDER BY id LIMIT 100001",
        )
        .await?,
    )
}

/// What a host needs to be sent one of `app`'s live versions for this campaign's job. Versions
/// the scheduler doesn't load, or that the gold hook refuses because they don't package the
/// job's engine, are left out.
pub(super) async fn requirements(c: &Config, app: &str) -> anyhow::Result<Requirements> {
    ensure!(
        crate::gold::submit::is_gold_app(app),
        "{app} is not a gold app"
    );
    let native = app == crate::gold::submit::NATIVE_APP;
    let download = c.project.join("download");
    let versions: Vec<Version> = loaded(parse_versions(
        &query(
            c,
            &format!(
                "SELECT p.name, av.plan_class, av.version_num, av.min_core_version, \
                 av.max_core_version, a.min_version, av.xml_doc \
                 FROM app_version av JOIN app a ON a.id=av.appid JOIN platform p ON p.id=av.platformid \
                 WHERE a.name='{app}' AND a.deprecated=0 AND av.deprecated=0 AND p.deprecated=0 \
                 ORDER BY av.id LIMIT 1001"
            ),
        )
        .await?,
    )?)
    .into_iter()
    .filter(|r| {
        packages_engine(&r.xml_doc, native, &c.engine.image, |name| {
            crate::read(&download.join(name), 1 << 20).ok()
        })
    })
    .map(|r| r.version)
    .collect();
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
            &crate::read(path, 1 << 20)
                .with_context(|| format!("reading engine_platforms {}", path.display()))?,
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

    fn host(id: u64, os: &str, vendor: &str, client: &str, docker: bool) -> Host {
        Host {
            id,
            os_name: os.into(),
            p_vendor: vendor.into(),
            client: client_version(client),
            docker: Some(docker),
            dont_use_docker: false,
            dont_use_wsl: false,
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
        <min_boinc_wsl_distro_version>2</min_boinc_wsl_distro_version>
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
        // A container but a client under the class's minimum.
        assert!(!gold.can_run(&host(13, "Linux Ubuntu", "AuthenticAMD", "8.2.9", true)));
        // A row with no misc doesn't show the host can't run it.
        let unknown = Host {
            client: None,
            docker: None,
            ..host(4, "Darwin", "Apple", "", false)
        };
        assert!(gold.can_run(&unknown));
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
    fn the_client_config_can_refuse_docker() {
        let gold = container_gold(None);
        let linux = host(9, "Linux Debian", "GenuineIntel", "8.2.13", true);
        assert!(gold.can_run(&linux));
        // misc.docker stays when the client config says dont_use_docker.
        let refused = Host {
            dont_use_docker: true,
            ..linux.clone()
        };
        assert!(!gold.can_run(&refused));
        // dont_use_wsl matters only on Windows.
        let no_wsl = Host {
            dont_use_wsl: true,
            ..linux
        };
        assert!(gold.can_run(&no_wsl));
        let windows = host(5, "Microsoft Windows 11", "AuthenticAMD", "8.2.11", true);
        assert!(windows.passes_docker());
        assert!(!Host {
            dont_use_wsl: true,
            ..windows.clone()
        }
        .passes_docker());
        // Windows needs Docker in an allowed WSL distro, whatever WSL distro version a class asks.
        assert!(!Host {
            docker: Some(false),
            ..windows
        }
        .passes_docker());
        // Docker on a Mac needs an 8.2.6 client.
        assert!(!host(1, "Darwin", "Apple", "8.2.5", true).passes_docker());
        assert!(host(1, "Darwin", "Apple", "8.2.6", true).passes_docker());
    }

    #[test]
    fn a_version_must_package_the_jobs_engine() {
        let image = format!("sha256:{}", "a".repeat(64));
        let reference = |file: &str, open: &str| {
            format!("<file_ref>\n  <file_name>{file}</file_name>\n  <open_name>{open}</open_name>\n</file_ref>\n")
        };
        let dockerfile = |payload: String| {
            move |name: &str| (name == "Dockerfile_x").then(|| payload.clone().into_bytes())
        };
        let from = format!("FROM ghcr.io/tgoodyear/usnm-boinc-worker@{image}");
        let doc = format!(
            "<file_info><name>Dockerfile_x</name></file_info>{}{}",
            reference("job_x.toml", "job.toml"),
            reference("Dockerfile_x", "Dockerfile")
        );
        assert!(packages_engine(
            &doc,
            false,
            &image,
            dockerfile(from.clone())
        ));
        assert!(packages_engine(
            &doc,
            false,
            &image,
            dockerfile(format!("{from}\n"))
        ));
        let other = "FROM ghcr.io/tgoodyear/usnm-boinc-worker@sha256:bbbb".to_string();
        assert!(!packages_engine(&doc, false, &image, dockerfile(other)));
        assert!(!packages_engine(&doc, false, &image, |_| None));
        // Two Dockerfiles, a repeated open name or an unsafe file name.
        let two = format!("{doc}{}", reference("Dockerfile_x", "Dockerfile"));
        assert!(!packages_engine(
            &two,
            false,
            &image,
            dockerfile(from.clone())
        ));
        let repeated =
            "<file_ref><file_name>Dockerfile_x</file_name><open_name>Dockerfile</open_name>\
                        <open_name>Dockerfile</open_name></file_ref>";
        assert!(!packages_engine(
            repeated,
            false,
            &image,
            dockerfile(from.clone())
        ));
        let unsafe_name = reference("../Dockerfile_x", "Dockerfile");
        assert!(!packages_engine(&unsafe_name, false, &image, |_| Some(
            from.clone().into_bytes()
        )));

        // A native build is its manifest's digest, which the physical name carries.
        let native = |digest: &str| {
            format!(
                "{}{}",
                reference("usnm_gold_native_1.0_x86_64-pc-linux-gnu", "usnm_ocr"),
                reference(
                    &format!("usnm_native_build_{digest}.json"),
                    "usnm_native_build.json"
                )
            )
        };
        assert!(packages_engine(
            &native(&"a".repeat(64)),
            true,
            &image,
            |_| None
        ));
        assert!(!packages_engine(
            &native(&"b".repeat(64)),
            true,
            &image,
            |_| None
        ));
        // A container version is no native build, and the reverse.
        assert!(!packages_engine(
            &doc,
            true,
            &image,
            dockerfile(from.clone())
        ));
        assert!(!packages_engine(
            &native(&"a".repeat(64)),
            false,
            &image,
            dockerfile(from)
        ));
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
                ..Default::default()
            }
        );
        assert_eq!(
            classes["mt"],
            PlanClass {
                docker: false,
                min_core_client_version: 0,
                max_core_client_version: 80299,
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
            "5\tMicrosoft Windows 11\tAuthenticAMD\t8.2.11\t0\t0\t1\n\
             9\tLinux Debian\tGenuineIntel\t8.2.13\t1\t1\t0\n\
             4\tDarwin\tApple\t\t\t0\t0\n",
        )
        .unwrap();
        assert_eq!(
            hosts[0],
            Host {
                dont_use_wsl: true,
                ..host(5, "Microsoft Windows 11", "AuthenticAMD", "8.2.11", false)
            }
        );
        assert_eq!(hosts[1].client, Some(80213));
        assert!(
            hosts[1].docker == Some(true) && hosts[1].dont_use_docker && !hosts[1].dont_use_wsl
        );
        assert_eq!((hosts[2].client, hosts[2].docker), (None, None));
        for bad in [
            "0\tLinux\tGenuineIntel\t8.2.13\t1\t0\t0",
            "1\tLinux\tGenuineIntel\t8.2.13\t1\t0",
            "1\tLinux\tGenuineIntel\t8.2.13\tNULL\t0\t0",
            "1\tLinux\tGenuineIntel\t8.2.13\t1\t0\ttrue",
            "1\tLinux\tGenuineIntel\t8.2.13\t1\t\t0",
            "1\tLinux\tGenuineIntel\t8.2.13\t1\t0\t0\n1\tLinux\tGenuineIntel\t8.2.13\t1\t0\t0",
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
        // mysql --batch writes a newline, tab or backslash in a value escaped.
        assert_eq!(
            parse_versions(
                "x86_64-pc-linux-gnu\tdocker\t104\t0\t0\t0\t<a>\\n\\t\\\\n\\x\\</a>\n\
                 x86_64-pc-linux-gnu\t\t100\t80211\t802\t0\t\n"
            )
            .unwrap(),
            vec![
                VersionRow {
                    version: version("x86_64-pc-linux-gnu", "docker"),
                    version_num: 104,
                    app_min_version: 0,
                    xml_doc: "<a>\n\t\\n\\x\\</a>".into(),
                },
                VersionRow {
                    version: Version {
                        min_core_version: 80211,
                        // As the scheduler reads a bound under 10000.
                        max_core_version: 80200,
                        ..version("x86_64-pc-linux-gnu", "")
                    },
                    version_num: 100,
                    app_min_version: 0,
                    xml_doc: String::new(),
                }
            ]
        );
        for bad in [
            "\tdocker\t1\t0\t0\t0\t",
            "x86_64-pc-linux-gnu\tdocker\t1\t0\t0\t0",
            "x86_64-pc-linux-gnu\tdo cker\t1\t0\t0\t0\t",
            "p\tdocker\t1\t-1\t0\t0\t",
            "p\tdocker\tNULL\t0\t0\t0\t",
        ] {
            assert!(parse_versions(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_scheduler_loads_only_the_latest_version_of_a_platform_and_plan_class() {
        let row = |platform: &str, plan_class: &str, num: u32, min: u32| VersionRow {
            version: version(platform, plan_class),
            version_num: num,
            app_min_version: min,
            xml_doc: format!("{num}"),
        };
        let docs = |rows: Vec<VersionRow>| -> Vec<String> {
            loaded(rows).into_iter().map(|r| r.xml_doc).collect()
        };
        // usnm_gold on 5 November 2026: 1.0 to 1.4 on x86-64 Linux, 1.3 and 1.4 on Apple silicon.
        let live = |min: u32| {
            let mut rows: Vec<_> = (100..=104)
                .map(|n| row("x86_64-pc-linux-gnu", "docker", n, min))
                .collect();
            rows.extend([103, 104].map(|n| row("arm64-apple-darwin", "docker", n, min)));
            rows.push(row("x86_64-pc-linux-gnu", "", 101, min));
            rows
        };
        assert_eq!(docs(live(0)), ["104", "104", "101"]);
        // With a min_version, every version from it on.
        assert_eq!(docs(live(103)), ["103", "104", "103", "104"]);
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
