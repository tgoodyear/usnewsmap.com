# Linux BOINC quick start

Donate spare compute to historical-newspaper OCR at
<https://boinc.usnewsmap.com/>. Start with one node, then repeat across your
allocation.

**Availability:** this is a limited pilot, not a guaranteed GPU work supply.
Linux native CUDA publication and end-to-end GPU acceptance have not been
verified for this guide. Attachments can wait for eligible work; ask the project
to confirm a Linux GPU campaign before allocating a cluster.

## 1. Prepare each node

- Install a current stable [official BOINC client](https://boinc.berkeley.edu/linux_install.php)
  (8.2.13+ recommended). Run its packaged service as the unprivileged BOINC user.
- For NVIDIA work, use x86-64 Linux and a driver supporting CUDA 12.4.
  Check `nvidia-smi` and `boinc --version`. Native Qwen needs at least
  3,000 MB available GPU memory for 2B, or 7,000 MB for 8B, plus two host CPUs
  per GPU task. These are scheduling minima, not a guarantee for every GPU.
- Allow at least 18 GiB of free BOINC disk space for native Qwen and model downloads,
  with additional headroom for concurrent tasks.
  Native apps need no Docker, Podman, CUDA toolkit or manual model installation.
  Container GPU apps need separate runtime setup; this quick start does not
  configure them.

Use **one client per node**, with its own persistent data directory. Do not
share a writable BOINC directory between nodes. On Slurm or another batch
scheduler, run only within an authorized allocation, restrict access to its
allocated CPUs and GPUs, and stop BOINC when it ends. Do not enable a permanent
service on shared nodes without administrator approval.

## 2. Create an account and attach

Create your account at <https://boinc.usnewsmap.com/>. Enable NVIDIA use in
project preferences; disable CPU work if you want GPU-only participation.
Set disk, memory and processing limits before requesting work.

In BOINC Manager, select **Add project**, enter that URL and sign in.
For headless nodes, use the repository's private-file attachment helper:

1. Obtain your **weak account key** from the account website. Store it in a
   private file with mode `0600`; do not put keys or passwords in shell commands,
   deployment logs or support messages.
2. From a current [repository checkout](https://github.com/tgoodyear/usnewsmap.com), run:

   ```sh
   sudo python3 scripts/boinc/client_account.py \
     --data-dir /var/lib/boinc \
     --expected-user-id YOUR_NUMERIC_USER_ID \
     --expected-user-name 'YOUR_ACCOUNT_NAME' \
     --key-file /secure/path/weak-account-key
   ```

   Substitute the service's actual data directory (some packages use
   `/var/lib/boinc-client`). Review the preview, then repeat with `--apply`.
   The client must be running; the helper uses local GUI-RPC and checks account
   identity. If already attached correctly, skip attachment rather than rejoining.

## 3. Verify and leave running

Run locally with permission to access the client's GUI-RPC credentials:

```sh
boinccmd --get_project_status
boinccmd --get_tasks
boinccmd --get_messages 0
```

Confirm the project URL, intended NVIDIA GPUs and successful scheduler contact.
Verify a task downloads, runs and uploads; check its validation status under
**Tasks** on your account website. Connection alone does not prove GPU execution.
No work may mean a closed campaign or no eligible application.

BOINC downloads signed apps and models and returns results and diagnostics.
Production and experimental work share your account and resource preferences;
there is no per-experiment enrollment. Select **No new tasks** to drain a node.
Pause processing when needed.

For container setup, troubleshooting and data handling, see the
[full volunteer reference](boinc-volunteer.md#11-linux-x86_64-pc-linux-gnu-and-aarch64-unknown-linux-gnu)
and [privacy details](boinc-volunteer.md#4-resources-network-and-privacy).
