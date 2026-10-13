# Contributing a machine

usnewsmap.com re-reads badly OCR'd historical newspaper pages with the Tesseract OCR engine. The
project plans to have contributed computers do part of this work through BOINC, the free
volunteer-computing client, which hands out pages and collects results.

Status: draft. No project server exists and `attach.sh` has not been run on a live host
(UNVERIFIED). The project address and sign-up details will come from the maintainer.

## What it uses

- One work unit is 4 newspaper pages. On the maintainer's nodes it takes about 11 to 22 minutes; it has not been timed on other hardware (UNVERIFIED).
- Each work unit needs up to 2.5 GiB of memory and about 200 MB of disk.
- It downloads about 18 MB of page images per work unit and uploads a small text result.
- The pages are scans from the Library of Congress's Chronicling America collection. The app
  reads only the page images it downloads.
- Planned first: 64-bit Linux (x86_64). Arm Linux is second. Windows and macOS are possible later.

## What it sends

The BOINC client tells the project your computer's CPU model, operating system, memory and disk
size, and its public IP address, as every BOINC project receives. The project keeps the
account name and email you give it. The project stores what BOINC stores: account name, email,
IP address, account key, and host and result rows (`design.md`, volunteer privacy). Whether the
client reports anything beyond the host details above is UNVERIFIED.

## Install and attach

You need the project address and an account key from the maintainer. On Debian, Ubuntu or
Fedora, as root:

```
sudo scripts/boinc/attach.sh --project-url https://PROJECT-ADDRESS/usnmocr/ \
    --account-key-file ./key.txt --max-cores 2 --max-memory-mib 6144 --run-if-idle
```

Add `--dry-run` first to see every step without changing anything. The script installs the
BOINC client from your distribution if it is missing, writes the limits below, and attaches.

| Flag                 | Meaning                                             | Default           |
| -------------------- | --------------------------------------------------- | ----------------- |
| `--max-cores N`      | most CPU cores BOINC may use                        | half the machine  |
| `--max-memory-mib N` | most memory BOINC may use (at least 2560)           | 40 percent of RAM |
| `--run-if-idle`      | work only after 3 minutes without keyboard or mouse | work whenever on  |

A work unit needs up to 2.5 GiB while a page is being read, so allow at least 2.5 GiB for each
core you give it. With less, BOINC is expected to fetch no work (not tested: UNVERIFIED).

Whether Rocky, Alma or RHEL carry `boinc-client` (possibly through EPEL) is UNVERIFIED. If
`dnf` cannot install it, the script stops: install the client yourself and run the script again.

## One account per person

BOINC does not send two copies of a work unit to hosts of the same account
(`one_result_per_user_per_wu`), so those hosts cannot check each other's results. Use one
account per person or per deployment, not one shared key for unrelated machines.

## Checking that it works

```
boinccmd --get_tasks
boinccmd --get_state
```

The first work unit should appear soon after attaching (not timed: UNVERIFIED). Credit is shown on the project's pages.

## Stop

```
sudo boinccmd --project https://PROJECT-ADDRESS/usnmocr/ detach
```

or run `attach.sh --project-url ... --detach`. To stop BOINC altogether:
`sudo systemctl disable --now boinc-client`. Work in progress is dropped. The server reissues it to another host after its 5-day deadline.

## Result checking

A new host's results are replicated and compared with another host's. A trusted host's results
are sampled. A result that does not match is not credited. The project's `ocr-sync` step checks
every result before use and sends rejects to quarantine (`design.md`). Sample rates and
thresholds are not chosen yet, and none of this has run.

## Fetching scans from the Library of Congress

Normally your machine downloads page images from the project's own storage and never contacts
the Library of Congress. The project may later offer a job type where your machine downloads the
scans from `tile.loc.gov` itself, at a low request rate. The source's servers would see your IP
address, with a User-Agent that names this project (`design.md` 4.4). A block by the source would
affect only your machine. The project accepts that risk. The type is off by default and not
offered today.

## Not tested

- `attach.sh` on a live host.
- Account-file pickup on a fresh client.
- `boinc-client` packages on RHEL-family distributions.
- Task timing on volunteer hardware.
- BOINC's behavior when memory is below the work unit's need.
