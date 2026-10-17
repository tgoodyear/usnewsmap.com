//! Immutable local publication and fresh public-download proof before write-ahead.
use anyhow::{ensure, Context};
use md5::{Digest, Md5};
use reqwest::{Client, Url};
use sha2::Sha256;
use std::{
    fs::File,
    io::{Read, Write},
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use usnm_ocr_contract::boinc::MAX_SCAN;

/// Explicitly opt in to local bulk hosting for the native OCR pilot.
pub struct Download {
    base: Url,
    root: Option<PathBuf>,
    #[cfg(test)]
    test_client: Option<Client>,
}

pub(super) struct Ready {
    pub url: String,
    pub bytes: String,
    pub md5: String,
}

/// Local publication receipt, not public-download readiness proof.
#[derive(Debug, serde::Serialize)]
pub struct PreparedDownload {
    pub sha256: String,
    pub bytes: u64,
    pub md5: String,
}

/// Run on the trusted local producer; never invokes BOINC or changes a journal.
pub fn prepare_download(page: &Path, download_root: &Path) -> anyhow::Result<PreparedDownload> {
    let scan = crate::read(page, MAX_SCAN)?;
    ensure!(!scan.is_empty(), "empty page");
    let sha256 = usnm_ocr_contract::boinc::sha256(&scan);
    publish(download_root, &sha256, &scan)?;
    Ok(PreparedDownload {
        sha256,
        bytes: scan.len() as u64,
        md5: hex(&Md5::digest(&scan)),
    })
}

impl Download {
    /// Publish on this producer as well as probing the public URL.
    pub fn new(base: &str, root: &Path) -> anyhow::Result<Self> {
        let mut download = Self::remote(base)?;
        open_root(root)?;
        download.root = Some(root.to_owned());
        Ok(download)
    }

    /// Probe a separately published origin without mounting it on the submitter.
    pub fn remote(base: &str) -> anyhow::Result<Self> {
        // Reject parser normalization (dot segments, escaped delimiters, backslashes)
        // and XML metacharacters before BOINC embeds the URL in workunit XML.
        ensure!(
            base.is_ascii()
                && !base
                    .bytes()
                    .any(|b| b.is_ascii_control() || b" %\\<>&\"'@".contains(&b)),
            "download URL must be unescaped ASCII without XML metacharacters"
        );
        let mut url = Url::parse(base)?;
        ensure!(
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "download base must be public HTTPS without credentials, query or fragment"
        );
        let raw_path = base
            .strip_prefix("https://")
            .context("HTTPS prefix required")?
            .split_once('/')
            .map_or("", |(_, path)| path);
        ensure!(
            raw_path
                .trim_end_matches('/')
                .split('/')
                .all(|p| p != "." && p != "..")
                && !raw_path.contains("//"),
            "unsafe download URL path"
        );
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        Ok(Self {
            base: url,
            root: None,
            #[cfg(test)]
            test_client: None,
        })
    }

    pub(super) async fn prepare(&self, scan: &[u8], sha256: &str) -> anyhow::Result<Ready> {
        ensure!(
            scan.len() <= MAX_SCAN && usnm_ocr_contract::boinc::sha256(scan) == sha256,
            "invalid page content address"
        );
        let url = self.base.join(sha256)?;
        ensure!(
            url.as_str().len() < 1024,
            "download URL too long for stock create_work"
        );
        if let Some(root) = &self.root {
            publish(root, sha256, scan)?;
        }
        #[cfg(not(test))]
        let client = public_client(&url).await?;
        #[cfg(test)]
        let client = if let Some(client) = &self.test_client {
            client.clone()
        } else {
            public_client(&url).await?
        };
        probe(&client, url.clone(), scan.len(), sha256).await?;
        Ok(Ready {
            url: url.into(),
            bytes: scan.len().to_string(),
            md5: hex(&Md5::digest(scan)),
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || a == 0
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 0 && c == 0))
        }
        IpAddr::V6(ip) => {
            // Global unicast only; reject mapped IPv4, NAT64, Teredo and 6to4.
            let s = ip.segments();
            s[0] & 0xe000 == 0x2000
                && !(s[0] == 0x2001 && s[1] < 0x200)
                && !(s[0] == 0x2001 && s[1] == 0xdb8)
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

async fn public_client(url: &Url) -> anyhow::Result<Client> {
    let host = url.host_str().context("missing host")?;
    let host = host.trim_matches(['[', ']']);
    let port = url.port_or_known_default().context("missing port")?;
    let name = host.to_owned();
    let addresses = resolve(
        move || {
            (name.as_str(), port)
                .to_socket_addrs()
                .map(Iterator::collect)
        },
        Duration::from_secs(10),
    )
    .await?;
    ensure!(
        !addresses.is_empty() && addresses.iter().all(|a| public_ip(a.ip())),
        "download host must resolve exclusively to public addresses"
    );
    Ok(Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        // Pin validated DNS results, avoiding a second lookup/rebinding.
        .resolve_to_addrs(host, &addresses)
        .build()?)
}

async fn resolve(
    lookup: impl FnOnce() -> std::io::Result<Vec<SocketAddr>> + Send + 'static,
    deadline: Duration,
) -> anyhow::Result<Vec<SocketAddr>> {
    let (send, receive) = tokio::sync::oneshot::channel();
    // System getaddrinfo cannot be cancelled. Keep it outside Tokio's blocking
    // pool so runtime shutdown cannot undo the CLI deadline. No project/journal
    // handles cross this boundary; late results are discarded after timeout.
    std::thread::Builder::new()
        .name("boinc-download-dns".into())
        .spawn(move || {
            let _ = send.send(lookup());
        })?;
    Ok(tokio::time::timeout(deadline, receive).await???)
}

async fn probe(client: &Client, url: Url, expected: usize, sha256: &str) -> anyhow::Result<()> {
    let mut response = client.get(url).send().await?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "download probe requires HTTP 200"
    );
    if let Some(n) = response.content_length() {
        ensure!(
            n == expected as u64 && n <= MAX_SCAN as u64,
            "download size mismatch"
        );
    }
    let mut bytes = 0usize;
    let mut hash = Sha256::new();
    while let Some(chunk) = response.chunk().await? {
        bytes = bytes
            .checked_add(chunk.len())
            .context("download size overflow")?;
        ensure!(
            bytes <= expected && bytes <= MAX_SCAN,
            "download exceeds page size"
        );
        hash.update(&chunk);
    }
    ensure!(
        bytes == expected && hex(&hash.finalize()) == sha256,
        "public download checksum/size mismatch"
    );
    Ok(())
}

#[cfg(unix)]
fn open_root(path: &Path) -> anyhow::Result<File> {
    use rustix::fs::{open, openat, Mode, OFlags};
    ensure!(
        path.is_absolute() && path.components().count() > 1,
        "download root must be an absolute directory, not /"
    );
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut fd = open("/", flags, Mode::empty())?;
    for part in path.components().skip(1) {
        let Component::Normal(name) = part else {
            anyhow::bail!("unsafe download root component");
        };
        fd = openat(&fd, name, flags, Mode::empty())?;
    }
    let file = File::from(fd);
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        file.metadata()?.permissions().mode() & 0o022 == 0,
        "download root must not be group/world writable"
    );
    Ok(file)
}

#[cfg(not(unix))]
fn open_root(_: &Path) -> anyhow::Result<File> {
    anyhow::bail!("local download publication requires Unix")
}

#[cfg(unix)]
fn publish(root: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    use rustix::fs::{linkat, openat, unlinkat, AtFlags, Mode, OFlags};
    use std::{os::unix::fs::PermissionsExt, sync::atomic::AtomicU64};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = open_root(root)?;
    let verify = || -> anyhow::Result<()> {
        let mut file = File::from(openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        ensure!(
            file.metadata()?.is_file(),
            "published input is not a regular file"
        );
        ensure!(
            file.metadata()?.len() == bytes.len() as u64,
            "published input size conflict"
        );
        ensure!(
            file.metadata()?.permissions().mode() & 0o222 == 0,
            "published input must be read-only"
        );
        let mut existing = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_SCAN as u64 + 1)
            .read_to_end(&mut existing)?;
        ensure!(existing == bytes, "published input content conflict");
        file.sync_all()?;
        directory.sync_all()?;
        Ok(())
    };
    // A fully written, synced inode is linked create-only; no partial public file
    // and no rename-overwrite, including when independent dispatch roots race.
    let (scratch, mut file) = scratch_file(&directory, &NEXT)?;
    let result = (|| -> anyhow::Result<()> {
        file.write_all(bytes)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o444))?;
        file.sync_all()?;
        match linkat(
            &directory,
            scratch.as_str(),
            &directory,
            name,
            AtFlags::empty(),
        ) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(e.into()),
        }
        verify()
    })();
    let cleanup = unlinkat(&directory, scratch.as_str(), AtFlags::empty());
    directory.sync_all()?;
    result?;
    cleanup?;
    Ok(())
}

#[cfg(unix)]
fn scratch_file(
    directory: &File,
    next: &std::sync::atomic::AtomicU64,
) -> anyhow::Result<(String, File)> {
    use rustix::fs::{openat, Mode, OFlags};
    use std::sync::atomic::Ordering;
    // Skip crash leftovers or another publisher's files without removing them.
    // Bound retries so an unexpectedly cluttered directory fails closed.
    for _ in 0..1024 {
        let name = format!(
            ".publish-{}-{}",
            std::process::id(),
            next.fetch_add(1, Ordering::Relaxed)
        );
        match openat(
            directory,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(fd) => return Ok((name, File::from(fd))),
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("too many interrupted publication scratch files")
}

#[cfg(not(unix))]
fn publish(_: &Path, _: &str, _: &[u8]) -> anyhow::Result<()> {
    anyhow::bail!("local download publication requires Unix")
}

#[cfg(all(test, unix))]
mod tests;
