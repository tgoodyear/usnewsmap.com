#!/usr/bin/env python3
"""Rent one Vast.ai GPU, train the #2107 layout detector, bring results back, destroy the instance.

Control plane only: this script makes Vast API calls and ssh commands to the dev container that holds
the dataset. The dataset goes from that container straight to the instance (tar over ssh, an
ephemeral key made on the container); results come back the same way. The API key is read from
Azure Key Vault into memory and never printed or written. The instance is destroyed in a finally
step in every case, and the API is asked to confirm it is gone.

Detector: Ultralytics YOLOv8 (AGPL-3.0), pinned; data prepared by build_dataset.py from finlam and
La Liberte (Teklia, MIT), Beyond Words (Library of Congress, CC0), with American Stories
(arXiv:2308.12477) teacher labels.

    vast_run.py --offer-id N --ct-host root@usnm-dev6.tjg.fyi --ct-dataset DIR --ct-out DIR
                [--ct-work DIR] [--budget 15] [--stop-at 13] [--ultralytics 8.4.208]
                [--image pytorch/pytorch:2.4.1-cuda12.4-cudnn9-runtime] [--disk 80]
"""
import argparse
import json
import shlex
import subprocess
import sys
import time
import urllib.error
import urllib.request

API = "https://console.vast.ai/api"


def api_key():
    return subprocess.check_output(["az", "keyvault", "secret", "show", "--vault-name", "home-kv-bmfr6iwvyzs52",
                                    "--name", "vastai-api-key", "--query", "value", "-o", "tsv"], text=True).strip()


class Vast:
    def __init__(self, key):
        self.key = key

    def call(self, method, path, body=None):
        req = urllib.request.Request(API + path, method=method, data=json.dumps(body).encode() if body is not None else None,
                                     headers={"Authorization": "Bearer " + self.key, "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                return json.load(r)
        except urllib.error.HTTPError as e:
            raise RuntimeError("Vast API %s %s: HTTP %d %s" % (method, path, e.code, e.read()[:300])) from None

    def instances(self):
        return self.call("GET", "/v1/instances/").get("instances", [])


def ct(host, cmd, check=True, user="nobody"):
    """Run a shell command on the container as `user` (root's su)."""
    full = "su -s /bin/sh %s -c %s" % (user, shlex.quote(cmd)) if user else cmd
    p = subprocess.run(["ssh", "-o", "ServerAliveInterval=30", host, full], capture_output=True, text=True)
    if check and p.returncode:
        raise RuntimeError("ct command failed (%d): %s\n%s" % (p.returncode, cmd[:200], p.stderr[-500:]))
    return p.stdout


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--offer-id", required=True, help="one offer id, or several separated by commas: the first still available is rented")
    ap.add_argument("--ct-host", required=True)
    ap.add_argument("--ct-dataset", required=True)
    ap.add_argument("--ct-out", required=True)
    ap.add_argument("--ct-work", default="/var/tmp/layout2107train")
    ap.add_argument("--code-dir", default="/var/tmp/layout2107train/code/layout_train")
    ap.add_argument("--budget", type=float, default=15.0)
    ap.add_argument("--stop-at", type=float, default=13.0)
    ap.add_argument("--ultralytics", default="8.4.208")
    ap.add_argument("--image", default="pytorch/pytorch:2.4.1-cuda12.4-cudnn9-runtime")
    ap.add_argument("--disk", type=int, default=80)
    ap.add_argument("--batch", type=int, default=16)
    a = ap.parse_args(argv)
    v = Vast(api_key())
    credit0 = v.call("GET", "/v0/users/current/")["credit"]
    print("credit before: %.4f" % credit0, flush=True)
    inst, t_start, dph = None, None, 0.0
    status = "started"
    try:
        body = {"client_id": "me", "image": a.image, "disk": a.disk, "label": "usnm-layout-2107",
                "runtype": "ssh_direc ssh_proxy", "env": "-e HOME=/root",
                "onstart": "touch /root/.no_auto_tmux /tmp/.no_auto_tmux"}
        for oid in a.offer_id.split(","):
            try:
                inst = v.call("PUT", "/v0/asks/%d/" % int(oid), body)["new_contract"]
                break
            except RuntimeError as e:
                print("offer %s unavailable: %s" % (oid, str(e)[-120:]), flush=True)
        if not inst:
            raise RuntimeError("no offer could be rented")
        t_start = time.time()
        print("instance", inst, flush=True)
        info = None
        for _ in range(120):
            time.sleep(15)
            rows = [i for i in v.instances() if i["id"] == inst]
            if rows and rows[0].get("actual_status") == "running" and rows[0].get("ports"):
                info = rows[0]
                break
        if not info:
            raise RuntimeError("instance never reached running")
        dph = float(info["dph_total"])
        host, port = info["public_ipaddr"], int(info["ports"]["22/tcp"][0]["HostPort"])
        print("running %s:%d  $%.3f/h  %s" % (host, port, dph, info.get("gpu_name")), flush=True)
        # Ephemeral key on the container.
        key = a.ct_work + "/vastkey"
        ct(a.ct_host, "rm -f %s %s.pub; ssh-keygen -q -t ed25519 -N '' -f %s" % (key, key, key))
        pub = ct(a.ct_host, "cat %s.pub" % key)
        v.call("POST", "/v0/instances/%d/ssh/" % inst, {"ssh_key": pub})
        ssh = "ssh -i %s -p %d -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ServerAliveInterval=30 root@%s" % (key, port, host)
        for _ in range(40):
            if "ok" in ct(a.ct_host, ssh + " echo ok", check=False):
                break
            time.sleep(15)
        else:
            raise RuntimeError("ssh to the instance never worked")
        ct(a.ct_host, ssh + " 'nvidia-smi --query-gpu=name,memory.total --format=csv; df -h /root | tail -1; nproc'")
        # Upload only the dataset and the training script.
        t0 = time.time()
        ct(a.ct_host, ssh + " 'mkdir -p /root/work/data'")
        ct(a.ct_host, "cd %s && tar cf - . | %s 'tar xf - -C /root/work/data'" % (a.ct_dataset, ssh))
        ct(a.ct_host, "cat %s/train_remote.py | %s 'cat > /root/work/train_remote.py'" % (a.code_dir, ssh))
        print("dataset uploaded in %.0fs" % (time.time() - t0), flush=True)
        ct(a.ct_host, ssh + " 'apt-get update -qq && apt-get install -y -qq libgl1 libglib2.0-0 >/dev/null 2>&1; pip install -q ultralytics==%s 2>&1 | tail -2'" % a.ultralytics)
        ct(a.ct_host, ssh + " 'cd /root/work && nohup python train_remote.py --data-root /root/work/data --out /root/work/out --batch %d > train.log 2>&1 &'" % a.batch)
        status = "training"
        while True:
            time.sleep(300)
            cost = (time.time() - t_start) / 3600 * dph
            tail = ct(a.ct_host, ssh + " 'tail -c 300 /root/work/train.log | tr \"\\r\" \"\\n\" | tail -2; ls /root/work/out/DONE 2>&1'", check=False)
            print("t=%.0f min  cost so far $%.2f\n%s" % ((time.time() - t_start) / 60, cost, tail.strip()), flush=True)
            if "/root/work/out/DONE" in tail and "No such file" not in tail:
                break
            if cost >= a.stop_at:
                raise RuntimeError("cost %.2f reached the stop line %.2f" % (cost, a.stop_at))
        t0 = time.time()
        ct(a.ct_host, "mkdir -p %s && %s 'cd /root/work && rm -f out/runs/train/weights/epoch*.pt && tar cf - train.log out' | tar xf - -C %s" % (a.ct_out, ssh, a.ct_out))
        print("results copied back in %.0fs" % (time.time() - t0), flush=True)
        status = "done"
    finally:
        wall = (time.time() - t_start) if t_start else 0
        if inst:
            for attempt in range(5):
                try:
                    v.call("DELETE", "/v0/instances/%d/" % inst)
                except Exception as e:  # noqa: BLE001
                    print("destroy attempt failed: %s" % e, flush=True)
                time.sleep(10)
                try:
                    if not any(i["id"] == inst for i in v.instances()):
                        print("instance %d is gone (API list)" % inst, flush=True)
                        break
                except Exception as e:  # noqa: BLE001
                    print("list failed: %s" % e, flush=True)
            else:
                print("WARNING: instance %d may still exist" % inst, flush=True)
        try:
            ct(a.ct_host, "rm -f %s/vastkey %s/vastkey.pub" % (a.ct_work, a.ct_work), check=False)
            credit1 = v.call("GET", "/v0/users/current/")["credit"]
            cost = {"credit_before": round(credit0, 4), "credit_after": round(credit1, 4),
                    "spent": round(credit0 - credit1, 4), "dph_total": dph, "wall_seconds": round(wall), "status": status}
            print(json.dumps(cost), flush=True)
            ct(a.ct_host, "mkdir -p %s && echo %s > %s/cost.json" % (a.ct_out, shlex.quote(json.dumps(cost)), a.ct_out), check=False)
        except Exception as e:  # noqa: BLE001
            print("cost report failed: %s" % e, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
