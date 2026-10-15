# shellcheck shell=bash
# Script-owned names outside ARM (ADR-0017). Also copied beside the
# standalone Proxmox script, so this file has no repository dependencies.
ocr_group_name() {
  printf 'usnm-ocr-nodes-grp-%s\n' "$1"
}

legacy_ocr_group_name() {
  printf 'usnm-ocr-nodes-%s\n' "$1"
}

valid_ocr_node_name() {
  local name=$1 env=${2:-}
  [[ $name =~ ^usnm-ocr-node[0-9]+-arcs-(prod|dev|x[a-z0-9]{1,4})$ ]] &&
    [ "${#name}" -le 32 ] &&
    { [ -z "$env" ] || [[ $name == *"-arcs-$env" ]]; }
}
