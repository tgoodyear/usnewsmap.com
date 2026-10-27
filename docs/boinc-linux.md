# Linux BOINC quick start

Donate your computer's spare time to historical-newspaper OCR at
<https://boinc.usnewsmap.com/>. You install your distribution's BOINC client,
join the project in BOINC Manager, and set limits. Nothing else to install for
the native apps: no Docker, Podman or CUDA toolkit.

**Availability:** this is a limited pilot. Work goes out in small batches, so a
joined computer can sit idle for a while. That is normal. Linux GPU work has
not yet been verified end to end on a volunteer host; ask the project before
setting up a whole cluster for it.

## 1. What you need

- 64-bit Linux on an Intel or AMD CPU. 64-bit ARM is accepted but not yet
  verified on a volunteer host.
- A BOINC client 8.2.13 or newer. Some distributions ship an older one; check
  with `boinc --version` and use [BOINC's own packages](https://boinc.berkeley.edu/linux_install.php)
  if yours is too old.
- For GPU work, an NVIDIA GPU with a driver that supports CUDA 12.4 or newer
  and at least 3 GB of free video memory (7 GB for the larger model), plus two free CPU
  cores per GPU task. Without one, the computer gets CPU work: the quality app
  on any machine, the Qwen CPU app on 4 to 8 cores.
- Free disk that BOINC is allowed to use: 2 GiB for the quality app, 12 GiB
  for the Qwen CPU app, and 18 GiB for the Qwen GPU app (those are binary
  gigabytes, so about 2.2, 13 and 20 GB as most disk tools show them).
  BOINC's own disk limit (**Options → Computing preferences → Disk**) must
  allow it as well.

## 2. Install BOINC and join the project

1. Install the client and Manager from your distribution (Debian and Ubuntu:
   `sudo apt install boinc-client boinc-manager`; Fedora and RHEL:
   `sudo dnf install boinc-client boinc-manager`). The client runs as a
   service under its own unprivileged `boinc` user.
2. Open **BOINC Manager**. If it cannot connect to the local client, add
   yourself to the `boinc` group (`sudo usermod -aG boinc "$USER"`), log out
   and back in, and try again.
3. Create an account at <https://boinc.usnewsmap.com/> (or sign in to yours).
   In **Project preferences** choose whether to use the GPU, the CPU or both.
4. In Manager, **Tools → Add project**, enter `https://boinc.usnewsmap.com/`
   and sign in with that account. Type your password only in Manager or on the
   project website, never into a script or a support message.

If the computer is already attached, do not remove and re-add the project.

**Headless machine or cluster node?** Attach with the repository's private-key
helper instead of Manager. Put your **weak account key** from the account
website in a file only you can read (mode `0600`), then from a
[checkout of this repository](https://github.com/tgoodyear/usnewsmap.com):

```sh
sudo python3 boinc/scripts/client_account.py \
  --data-dir /var/lib/boinc \
  --expected-user-id YOUR_NUMERIC_USER_ID \
  --expected-user-name 'YOUR_ACCOUNT_NAME' \
  --key-file /secure/path/weak-account-key
```

Review the preview, then repeat with `--apply`. Use the service's real data
directory (some packages use `/var/lib/boinc-client`). Run one client per
node with its own data directory; on a batch scheduler, run only inside an
allocation you are authorised for and stop BOINC when it ends.

## 3. Set limits

In Manager, **Options → Computing preferences**: compute only while the
computer is idle and not on battery, and set CPU, memory and disk limits you
are comfortable with. These apply to every BOINC project on this computer. On
a headless machine, Manager can connect to it from another computer, or set
the same limits in **Computing preferences** on the project website (not
Project preferences, which only choose CPU and GPU work for this project).
Website limits apply to every host on your account; limits set locally in
Manager take precedence over them.

## 4. Check that it works

```sh
boinccmd --get_project_status
boinccmd --get_tasks
boinccmd --get_messages 0
```

The first should list `https://boinc.usnewsmap.com/`. A **Scheduler request
completed** line for usnewsmap in the messages means you are joined and
waiting for a batch.
A task that downloads, runs and uploads is a success; its validation shows
under **Tasks** on your account page. For GPU work, the messages at startup
should name the NVIDIA GPU; without that line the computer gets CPU work only.

If `boinccmd` is refused, run it as the `boinc` user or from the data
directory, where the client's password file is; never copy that password into
a command.

## 5. Pause, stop or leave

- **Pause:** Manager's **Activity** menu, or the project's **Suspend** button
  (`boinccmd --project https://boinc.usnewsmap.com/ suspend`).
- **Stop taking work:** select the project and click **No new tasks**
  (`... nomorework`); running tasks finish and report.
- **Leave:** select the project and click **Remove** (`... detach`). Delete
  your account on the website if you also want that gone.

BOINC downloads signed apps and models by itself and returns results and
diagnostics. Never copy in executables or models or edit `app_info.xml`; if a
task fails, BOINC reports it and the project sees it.

The container apps (`usnm_ocr`, `usnm_assess`, `usnm_gold`, `usnm_surya`) need
Podman and the repository's rootless profile; the
[full volunteer reference](boinc-volunteer.md#11-linux-x86_64-pc-linux-gnu-and-aarch64-unknown-linux-gnu)
covers that setup, troubleshooting, and
[what the project sees from your computer](boinc-volunteer.md#4-resources-network-and-privacy).
