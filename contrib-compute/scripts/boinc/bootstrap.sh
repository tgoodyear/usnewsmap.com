#!/usr/bin/env bash
# Operator script for the BOINC server: plans the Bicep deployment and the steps that follow
# it. In this version it can only preview. Nothing is deployed and no secret is created;
# --what-if is required, and the script refuses to run without it.
#
#   bootstrap.sh --what-if --resource-group RG --image REGISTRY/NAME@sha256:... \
#                --signing-key-dir DIR [--location eastus] [--name boinc]
#
# What a real run will do, in order (printed below as the plan):
#   1. az deployment group what-if / create for infra/boinc/main.bicep
#   2. generate the MySQL, upload-key and runtime secrets and store them in Key Vault
#   3. create the database schema: make_project on first run, `upgrade` on later runs
#   4. build the app files and sign them OFFLINE, then register the version with update_versions
#   5. check that the project's get_project_config.php answers
#
# The code-signing PRIVATE key never comes to this script or to Azure. --signing-key-dir
# names a directory on your offline machine (for example a mounted USB stick). The script
# only checks that it holds code_sign_private and prints the signing commands for you to run
# there; the signed files (*.sig) come back to this machine.
set -euo pipefail

SCRIPT_VERSION="0.1.0"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

die() { echo "bootstrap.sh: $*" >&2; exit 1; }
note() { echo "== $*"; }

usage() {
  cat <<'USAGE'
Usage: bootstrap.sh --what-if --resource-group RG --image IMAGE --signing-key-dir DIR [options]

Required:
  --what-if              preview only. This version cannot deploy, so the flag is mandatory
  --resource-group RG    Azure resource group for the project
  --image IMAGE          server image, by digest: registry/name@sha256:<64 hex>
  --signing-key-dir DIR  offline directory that holds code_sign_private (it is only checked
                         for, never read or copied)

Options:
  --location NAME        Azure region (default eastus)
  --name NAME            short resource name prefix, 3 to 12 lower-case letters/digits (default boinc)
  --project-url URL      public master URL, https:// with trailing slash (default: the Container App address)
  --help                 this text
  --version              print the script version
USAGE
}

what_if=0 rg="" image="" key_dir="" location="eastus" name="boinc" project_url=""

while [ $# -gt 0 ]; do
  case "$1" in
    --what-if) what_if=1; shift ;;
    --resource-group) [ $# -ge 2 ] || die "$1 needs a value"; rg="$2"; shift 2 ;;
    --image) [ $# -ge 2 ] || die "$1 needs a value"; image="$2"; shift 2 ;;
    --signing-key-dir) [ $# -ge 2 ] || die "$1 needs a value"; key_dir="$2"; shift 2 ;;
    --location) [ $# -ge 2 ] || die "$1 needs a value"; location="$2"; shift 2 ;;
    --name) [ $# -ge 2 ] || die "$1 needs a value"; name="$2"; shift 2 ;;
    --project-url) [ $# -ge 2 ] || die "$1 needs a value"; project_url="$2"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    --version) echo "$SCRIPT_VERSION"; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

[ "$what_if" -eq 1 ] || die "this version only previews: pass --what-if. Nothing is deployed."
[ -n "$rg" ] || die "--resource-group is required"
[ -n "$image" ] || die "--image is required"
[ -n "$key_dir" ] || die "--signing-key-dir is required"
case "$image" in *@sha256:[0-9a-f]*) ;; *) die "--image must be pinned by digest (…@sha256:<64 hex>)" ;; esac
[ "${#image}" -gt 71 ] || die "--image digest looks too short"
case "$name" in *[!a-z0-9]*|'') die "--name must be lower-case letters and digits" ;; esac
[ "${#name}" -ge 3 ] && [ "${#name}" -le 12 ] || die "--name must be 3 to 12 characters"
if [ -n "$project_url" ]; then
  case "$project_url" in https://*/) ;; *) die "--project-url must start with https:// and end with /" ;; esac
fi

# The signing key must be offline: refuse a path inside this repository.
key_abs="$(cd "$key_dir" 2>/dev/null && pwd)" || die "--signing-key-dir $key_dir does not exist"
case "$key_abs/" in "$repo_root"/*) die "--signing-key-dir is inside this repository; use an offline location" ;; esac
[ -f "$key_abs/code_sign_private" ] || die "no code_sign_private in $key_abs"
if [ -n "$(find "$key_abs/code_sign_private" -perm -004 2>/dev/null)" ]; then
  echo "bootstrap.sh: warning: code_sign_private is readable by everyone; chmod 600 it" >&2
fi

note "plan only; nothing below is executed"
echo
note "1. Preview the Azure deployment (read-only; run it yourself)"
cat <<PLAN
az deployment group what-if \\
  --resource-group $rg \\
  --template-file $repo_root/infra/boinc/main.bicep \\
  --parameters name=$name location=$location image=$image \\
               mysqlAdminPassword=<generated, from Key Vault> filesAccountKey=<read from the files account>
PLAN
echo
note "2. Secrets (real run): generated locally, written straight to Key Vault, never printed"
echo "   mysql-admin-password, mysql-runtime-password, and the upload key pair (make_keys output)"
echo "   Key Vault name comes from the deployment output keyVaultName."
echo
note "3. Database schema (real run, inside the Container App via az containerapp exec)"
echo "   first run:  make_project --no_query --db_host <mysql host> ... usnmocr   (creates the schema)"
echo "   later runs: bin/upgrade   (applies schema updates from a newer BOINC release)"
echo "   then:       bin/xadd < boinc/project.xml   (registers app usnm_ocr and the platforms)"
echo
note "4. App version and code signing (real run; the private key stays offline)"
cat <<PLAN
   On the build machine: build the pinned static tesseract, usnm-ocr-run and the wrapper,
   put them in apps/usnm_ocr/1.00/x86_64-pc-linux-gnu/ with job.xml and version.xml.
   On the OFFLINE machine ($key_abs), for each file F in that directory:
       bin/sign_executable F $key_abs/code_sign_private > F.sig
   Copy the .sig files back beside the files, then in the container:
       bin/update_versions
   If update_versions finds no code_sign_private, it uses the .sig files. That is the intended path.
PLAN
echo
note "5. Check (real run)"
url="${project_url:-https://<container app address>/}"
echo "   curl -fsS ${url}usnmocr/get_project_config.php"
echo "   workunit count, feeder log, one test result through usnm-boinc-validate"
echo
note "done: previewed only. Deploy with az yourself when the open questions in docs/design.md are settled."
