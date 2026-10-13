# Running a node on Proxmox

Private. For people the maintainer has vetted and given a project address and an account key.

You run one script as root on your Proxmox host. It makes an ordinary unprivileged Debian
container, installs Debian's `boinc-client` package in it and points that client at the usnewsmap
BOINC project. The project provides no custom template or image.

## What you need

- Proxmox VE with a Debian 12 container template available (the script downloads the stock one
  with `pveam`) and outbound HTTPS from the container.
- From the maintainer: the project address (`https://.../`, ends with a slash) and a BOINC account
  key. Put the key in a file, one line, mode 600:

      install -m 600 /dev/null /root/usnm-account-key
      printf '%s\n' '<your key>' > /root/usnm-account-key

  One key per container group. Do not share a key with someone else's node: hosts of one account
  cannot check each other's results (`docs/interface.md`).

- The Proxmox firewall switched on at datacenter level. The LAN block below does nothing without it.

## Running the script

    scripts/enroll.sh --dry-run --project-url https://<project-host>/<project>/ \
      --account-key-file /root/usnm-account-key --ctid 9100 --storage local-lvm --bridge vmbr0 \
      --cores 4 --memory-mib 10240 --block-cidr <your LAN range>

`--dry-run` prints every command and file, shows the key as `<key>` and runs nothing. Remove it to
do the real thing. Flags:

| Flag                                        | Meaning                                                              |
| ------------------------------------------- | -------------------------------------------------------------------- |
| `--project-url`, `--account-key-file`       | Where to attach and the account to attach with                       |
| `--cores`, `--memory-mib`                   | Container size. Tasks at once = memory / 2560 MiB, at most the cores |
| `--max-disk-gib`                            | Most disk BOINC may use for project files (default 5)                |
| `--run-if-idle`                             | Compute only when idle (waits 3 minutes; default is always on)       |
| `--block-cidr` (repeat) or `--no-lan-block` | LAN ranges the container may not reach                               |
| `--disk-gib`, `--hostname`, `--template*`   | Container disk and name; template choice                             |

## What the script does, in order

1. `pveam update` and downloads the newest stock `debian-12-standard` template if you lack it.
2. `pct create`: unprivileged, no password, no ssh keys, no bind mounts, swap off, start on boot.
   No `nesting` or `keyctl` features: the app is native (no Docker, no podman), so the client needs
   neither.
3. Writes `/etc/pve/firewall/<ctid>.fw`: inbound dropped, and `OUT DROP -dest <range>` for each
   `--block-cidr`. Everything else outbound is allowed.
4. Starts the container, then `apt-get install --no-install-recommends boinc-client` inside it.
5. Stops the client, writes `global_prefs_override.xml` (from the table in
   `docs/interface.md`, with `--max-disk-gib` and `--run-if-idle` applied and the CPU
   percentage computed from `--cores`/`--memory-mib`) and an `account_<project>.xml` file in the
   client's data directory, then starts the client. The key is in that file (mode 600, owned by
   the `boinc` user). It is never on a command line or in the dry-run output (checked by dry run). Whether it stays
   out of the logs on a real run is UNVERIFIED. The
   file name rule is taken from the BOINC source (`lib/url.cpp`, `client/file_names.cpp`, read
   2026-10-13).

## Status and removal

    pct exec 9100 -- boinccmd --get_project_status
    pct exec 9100 -- boinccmd --get_tasks
    pct stop 9100                              # pause everything
    pct stop 9100; pct destroy 9100 --purge    # remove the container and its disk
    rm -f /etc/pve/firewall/9100.fw            # if it is still there

Removing the container and its firewall file leaves the downloaded Debian template in your template storage. Removal has not been run on a real host (UNVERIFIED). Delete the key file when you are done.

## What the maintainer sees

Your host appears in BOINC as a host under your account: name, OS, CPU, memory, tasks done and
credit. It also sees your public IP address, because that is how clients reach any BOINC server.
It cannot log in to your container, but the BOINC client runs the project's signed app inside it. Whether the client reports a LAN address is UNVERIFIED (`docs/verified-facts.md` lists only the public IP). The container has no listening port.

## Limits

- The container's network is only as isolated as your bridge and firewall. Unprivileged LXC shares
  the host kernel; this is not a VM boundary. If that matters, run the Azure or VM route instead.
- Tasks are 11 to 22 minutes each on the maintainer's hardware (not measured on yours: UNVERIFIED), up to 2.5 GiB RAM, 200 MB disk, 18 MB download (`docs/interface.md`).

## Sources and what is not verified

Read 2026-10-13 (US Eastern):

- BOINC wiki, `Installing BOINC on Debian or Ubuntu`, `Boinccmd-tool`, `Global prefs override.xml`,
  `ClientFiles`, `Creating-custom-installers` (account files copied in advance): clone of
  `https://github.com/BOINC/boinc.wiki.git`, commit `7e16379`.
- BOINC source `https://github.com/BOINC/boinc` at `0f611c6`: `lib/url.cpp`,
  `client/file_names.cpp`, `client/cs_account.cpp`, `lib/prefs.cpp`.
- Debian package `boinc-client` (bookworm): `https://packages.debian.org/bookworm/boinc-client`.
  The Debian file list shows the data directory as `/var/lib/boinc` while the BOINC wiki says
  `/var/lib/boinc-client`. The script avoids the question by using the home directory of the
  package's `boinc` user.

Not tested on a real Proxmox host (UNVERIFIED until someone runs it):

- That the client picks up a pre-placed `account_*.xml` when it has no `client_state.xml` yet.
  The source reads account files at start (`client_state.cpp` calls `parse_account_files`), and the
  wiki describes copying them in for installers, but no run has confirmed it. Fallback:
  `pct exec <ctid> -- boinccmd --project_attach <url> <key>`, which puts the key in that command's
  argument list for the length of the call.
- That `nproc` inside the container reflects `--cores` (depends on LXCFS and how Proxmox applies
  the core limit). If it shows the host's CPUs the percentage is computed from that number, and
  the memory limit then bounds tasks.
- That `--no-install-recommends` leaves a working client on bookworm.
- The container firewall rules on your Proxmox version. Test: from inside the container, try to
  reach a LAN address and expect it to fail.
- No Proxmox-specific BOINC guide was found. Proxmox VE Helper-Scripts
  (`https://github.com/community-scripts/ProxmoxVE`) was checked on 2026-10-13; this script does not
  depend on it.
