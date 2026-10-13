# Running a node in a container (Docker or Podman)

Private. For people the maintainer has vetted and given a project address and an account key.

One script, `scripts/run-container.sh`, runs the official BOINC client image (`boinc/client`) with
your limits and points it at the usnewsmap BOINC project. We build no image of our own.

## What you need

- Docker or Podman on Linux (x86_64 for now; see the platform notes).
- From the maintainer: the project address (`https://.../`, ends with a slash), a BOINC account key,
  and ideally an image digest. Put the key in a file, mode 600:

      install -m 600 /dev/null ~/usnm-account-key
      printf '%s\n' '<your key>' > ~/usnm-account-key

## Run it

    scripts/run-container.sh --dry-run --project-url https://<project-host>/<project>/ \
      --account-key-file ~/usnm-account-key --cores 4 --memory-mib 10240 \
      --block-cidr <your LAN range> --image-digest sha256:<digest from the maintainer>

Drop `--dry-run` to start it. The dry run prints every command and file, shows the key as `<key>`,
and runs nothing.

| Flag                                       | Meaning                                                                                       |
| ------------------------------------------ | --------------------------------------------------------------------------------------------- |
| `--engine docker\|podman\|auto`            | Default auto: Docker if found, then Podman                                                    |
| `--cores`, `--memory-mib`                  | Limits (`--cpus`, `--memory`, swap off). Tasks at once = memory / 2560 MiB, at most the cores |
| `--max-disk-gib`, `--run-if-idle`          | BOINC disk cap (default 5) and idle-only running                                              |
| `--image-digest`                           | Run exactly that digest; the pull fails if it differs                                         |
| `--image-archive`, `--archive-sha256`      | Load a saved image file after checking its sha256, instead of pulling                         |
| `--quadlet`                                | Podman only: install a systemd (Quadlet) service so it starts at boot                         |
| `--no-hardening`                           | Skip `--cap-drop ALL`, `no-new-privileges`, read-only root file system                        |
| `--status`, `--stop`, `--remove [--purge]` | Manage it. `--purge` also deletes the data volume and network                                 |

What it sets up: a private network and a named volume (`<name>-net`, `<name>-data`), the client's
preferences (`global_prefs_override.xml`, values from `docs/interface.md`) and an account file
copied into the volume through a throwaway container, so the key is never in a command line. The
client's local RPC port is not published; its password is random. Restart: Docker and Podman get
`--restart unless-stopped`; with Podman that survives a reboot only if
`podman-restart.service` is enabled, or use `--quadlet`.

## Blocking your LAN

The script prints rules but does not apply them: it cannot, in rootless setups, and a wrong guess
would give a false sense of safety. Apply them yourself and test.

- Docker on Linux, UNVERIFIED and not run: `iptables -I DOCKER-USER -s <network subnet> -d <your LAN range> -j DROP`,
  as root, one rule per range. `DOCKER-USER` is the chain Docker documents for user rules; the
  2026-10-13 docs fetch did not return that page, so check your Docker version's docs.
  Rules vanish at reboot unless you persist them. With nftables or firewalld the form differs.
- Rootful Podman: host firewall rules on the network's subnet. UNVERIFIED; chain names depend on
  your setup.
- Rootless Podman: UNVERIFIED. Traffic leaves through your user's own network stack, so a subnet
  rule may not match. A host firewall rule keyed on your user may work. If you cannot enforce it,
  use the Proxmox route or a separate VM.
- Docker Desktop (Mac, Windows): the rules would have to go inside Docker's VM. Not supported by this script.
- Test it: `<engine> exec <name> bash -c 'timeout 3 bash -c "</dev/tcp/<a LAN host>/80"; echo $?'`
  and expect a failure.

A container is not a VM. It shares your kernel, and the official image runs as root inside the
container (below). The hardening flags reduce what that root can do but are untested here.

## What the maintainer sees

Your host as a BOINC host (name, OS, CPU, memory, tasks, credit) and your public IP address.
Whether the client reports a LAN address is UNVERIFIED. The client only makes outbound
connections but runs the project's signed app.

## Official image

Read 2026-10-13 from `https://github.com/BOINC/boinc-client-docker` (README, `Dockerfile.base-ubuntu`,
`bin/start-boinc.sh`) and the Docker Hub tag list `https://hub.docker.com/v2/repositories/boinc/client/tags`:

- It is `ubuntu:latest` plus the `boinc-client` package from a third-party PPA
  (`ppa:costamagnagianfranco/boinc`), not from Ubuntu or Debian. Trust in that PPA is trust in its
  publisher (`costamagnagianfranco`). Debian's own package is the alternative (the Proxmox route uses it).
- The tags `latest`, `arm64v8` and `arm32v7` were last pushed 2025-05-04, so the image is over a
  year old at the time of reading. `apt` updates inside a long-lived container are not run. Pin a
  digest the maintainer has checked, and plan to refresh it.
- It runs as root and has no `USER`. Its default RPC password is `123`; this script replaces it.
  The README examples use `--net=host --pid=host`; this script does not.
- `--cap-drop ALL`, `--read-only` and `no-new-privileges` are our additions. They are UNVERIFIED
  with this image: the entrypoint writes `gui_rpc_auth.cfg` and `remote_hosts.cfg` into the volume
  and starts `/usr/bin/boinc`, which should work, but nobody has run it. If the container exits at
  once, run again with `--no-hardening` and tell the maintainer.

## Platform notes

- The app is native (`docs/interface.md`): x86_64 Linux first, aarch64 Linux second. A Mac with
  Apple silicon, a Raspberry Pi or another ARM host gets the `arm64v8` image, but the client will
  receive no work until an aarch64 app version exists. 32-bit ARM (`arm32v7`): no app version planned.
- Docker Desktop (Mac, Windows) runs containers in a VM. `--cpus` and `--memory` cannot exceed the
  VM's size, the script reads the VM's CPU count, and the LAN block cannot be applied (above). Whether
  the official image works there at all is UNVERIFIED.
- Docker-wrapper BOINC apps need a Docker daemon the client can reach on the host
  (`docs/verified-facts.md` (k)). They are not expected to work inside this container (nesting
  UNVERIFIED by test). That is why the project's app is native.

## Remove everything

    scripts/run-container.sh --remove --purge --engine docker --name usnm-contrib
    rm ~/usnm-account-key

## Not verified

Nothing here has been run on a real Docker or Podman host: the `create` + `cp` seeding step, the
hardening flags, `--cpus`/`--memory` in rootless mode (cgroup v1 rootless does not support swap
limits, Podman docs), Quadlet start, whether the client picks up a pre-placed `account_*.xml` on a
fresh data volume, and the LAN rules above.
