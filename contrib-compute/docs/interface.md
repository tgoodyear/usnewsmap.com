# BOINC interface for the host routes

For the Proxmox container, the docker/podman container and the Azure Spot VM or container
group scripts. They run a BOINC client attached to the project server. This page lists the
names they can rely on. Design reasons are in [design.md](design.md).

Status: names below are fixed for the first version. Nothing is deployed; the project URL is a
placeholder that the maintainer fills in when the server exists.

The scripts that use this page are the client scripts for Proxmox, Docker or Podman, Azure and
plain Linux machines. They are in a follow-up pull request, not in this directory yet, and the
script names below are the planned ones.

## Names

| Item                   | Value                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Project URL            | the flag `--project-url URL` (a `https://` URL with a trailing slash). No default, because nothing is published. Not an environment variable                                                                                                                                                                                                                                                                                                               |
| Account key            | the flag `--account-key-file FILE`: a file holding the BOINC authenticator, mode 600. The key is never an environment variable, a command-line argument, a unit file or part of an image                                                                                                                                                                                                                                                                   |
| App name               | `usnm_ocr`                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| App version            | `1.00` (`version_num` 100), no plan class                                                                                                                                                                                                                                                                                                                                                                                                                  |
| Platforms              | `x86_64-pc-linux-gnu` first; `aarch64-unknown-linux-gnu` second; Windows and Mac later, if at all                                                                                                                                                                                                                                                                                                                                                          |
| App type               | native: a signed `ocr-run` driver and a static `tesseract`, started by BOINC's `wrapper`. No Docker, no VirtualBox                                                                                                                                                                                                                                                                                                                                         |
| Client needs           | BOINC client 7.x or later with the generic wrapper support (any distribution package). No Docker, no GPU, no privileged mode                                                                                                                                                                                                                                                                                                                               |
| Client network         | outbound HTTPS to the project URL, and to the staging blob host named in each workunit (and to `tile.loc.gov` only if the maintainer ever enables fetch-and-ocr, design.md 4.4). No inbound ports                                                                                                                                                                                                                                                          |
| Stops and eviction     | the app checkpoints once per finished page (`checkpoint/page<N>.json` in the task's slot directory) and resumes from the next page. A Spot eviction notice is 30 s and a page takes 60 to 250 s, so a stop loses at most the page in progress. On an ephemeral disk that is discarded at eviction the task is lost and retried after its deadline. Keep the BOINC data directory on a disk that survives an eviction if the host allows it (design.md 5.3) |
| Per-task resources     | one CPU core, up to 2.5 GiB RAM, 200 MB disk. Workunit: 4 pages, 11 to 22 minutes on a node like the maintainer's                                                                                                                                                                                                                                                                                                                                          |
| Download per workunit  | about 18 MB of page images, plus the app files once per version                                                                                                                                                                                                                                                                                                                                                                                            |
| Account creation       | `boinccmd --create_account URL email password name`, or attach with an existing key: `boinccmd --project_attach URL key`. Whether account creation is open or needs an invite code is the maintainer's choice (see design.md 5.5)                                                                                                                                                                                                                          |
| One key per deployment | do not share one account key across unrelated deployments: hosts of one account cannot validate each other's results (`one_result_per_user_per_wu`)                                                                                                                                                                                                                                                                                                        |

## Client preference defaults

Written to `global_prefs_override.xml` in the BOINC data directory (on Debian and Ubuntu,
`/var/lib/boinc-client/`), then `boinccmd --read_global_prefs_override`. Memory limits in BOINC
are percentages of RAM, not megabytes.

The values live in one place: `boinc_prefs_xml` in `scripts/lib/boinc-common.sh`. The Azure
script `scripts/azure/boinc-config.sh` is embedded into Bicep and cloud-init and cannot source
that file, so it carries a copy; `scripts/check-prefs-consistent.sh` runs in CI and fails if the
two differ. The scripts' flags are canonical: where this document and a script's `--help`
disagree, the script wins.

What the helper sets by default:

- CPU: `max_ncpus_pct` from the number of tasks wanted (`--cores`, `--tasks`, or
  `--max-cores`), `cpu_usage_limit` 100.
- RAM: `ram_max_used_busy_pct` 80 and `ram_max_used_idle_pct` 90 on a dedicated host.
- Disk: `disk_max_used_gb` from `--max-disk-gib` (the dedicated-host scripts default to 5).
- Running while the user is active: allowed by default; `--run-if-idle` sets
  `run_if_user_active` 0 and `idle_time_to_run` 3 (minutes).
- `work_buf_min_days` 0.1, `work_buf_additional_days` 0.25, `suspend_cpu_usage` 0.
- `run_on_batteries` 1.

`scripts/boinc/attach.sh` is for plain machines and changes three things: the RAM limits are the
same percentage for busy and idle (from `--max-memory-mib`, default 40 percent of RAM), the disk
limit is 2 GiB, and `run_on_batteries` is 0. It defaults to half the cores (`--max-cores`).

`work_buf_*` are kept small so a host holds about a day of work at most. Spot VMs can vanish; a
workunit lost that way is retried by BOINC after the 5-day deadline, so keep the buffer small.

## Layout of `contrib-compute/`

| Path                                                                                             | What                                                                  |
| ------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------- |
| `docs/verified-facts.md`, `design.md`, `interface.md`, `volunteers.md`, `ocr-sync-provenance.md` | Documents                                                             |
| `infra/boinc/`                                                                                   | Bicep for the server side (compile and lint only)                     |
| `boinc/project.xml`, `boinc/config.xml.template`                                                 | Project definition; no secrets                                        |
| `boinc/apps/usnm_ocr/`                                                                           | Job template, input and output templates, `job.xml`                   |
| `boinc/` (Rust crate `usnm-boinc`)                                                               | Work generator, validator, assimilator, `ocr-run`                     |
| `scripts/boinc/attach.sh`                                                                        | Volunteer script: install or detect the client, attach, set limits    |
| `scripts/boinc/bootstrap.sh`                                                                     | Operator script: Bicep deployment and post-deploy steps, what-if only |

## What the host scripts should do

1. Install `boinc-client` from the distribution (or use `scripts/boinc/attach.sh`, which does).
2. Take `--project-url` and `--account-key-file`. Before the client starts, write the account
   file (`boinc_account_filename` in `scripts/lib/boinc-common.sh` gives the name) in the BOINC data directory (mode 600, owned by the
   `boinc` user):

   ```xml
   <account>
     <master_url>URL</master_url>
     <authenticator>KEY</authenticator>
   </account>
   ```

   The client reads it at start and attaches, so the key is never on a command line. The file
   name rule (no scheme, every other character that is not a letter, digit, `.`, `-` or `_`
   becomes `_`, one trailing `_` dropped) comes from reading the BOINC source and is UNVERIFIED
   on a running client; `attach.sh` checks
   with `boinccmd --get_project_status` after the restart and says so if the project is not
   attached. On a host with no way to place a file before start, `boinccmd --project_attach`
   puts the key in that process's arguments for a moment; do that only on a single-user host.

3. Write the preferences above and run `boinccmd --read_global_prefs_override`.
4. Start the `boinc-client` service. Nothing else is required in the guest: no registry login,
   no storage key, no Azure credential.
5. On a Spot VM, put the data directory on a disk that survives eviction if possible, and do
   not delay the stop of the client: it needs no clean shutdown for the checkpoints to hold.
