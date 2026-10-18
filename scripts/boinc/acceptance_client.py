"""Secret-free client probe, executed on an explicitly dedicated test client."""
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET


def probe(root, url):
    state = ET.parse(root / 'client_state.xml').getroot()
    project = next((p for p in state.findall('project') if p.findtext('master_url') == url), None)
    if project is None or not int(project.findtext('hostid', '0')):
        return {'ready': False}
    host = state.find('host_info')
    prefs = {}
    for name in ('global_prefs.xml', 'global_prefs_override.xml'):
        path = root / name
        if path.exists():
            tree = ET.parse(path).getroot()
            # Venue-specific preferences cannot be inferred safely by this probe.
            if tree.findall('venue'):
                raise ValueError('venue preferences require dedicated-client override')
            prefs.update({n.tag: n.text for n in tree if n.text})
    cpus = int(host.findtext('p_ncpus', '0'))
    fraction = min(float(prefs.get('max_ncpus_pct', 100)), 100) / 100
    usable = max(1, math.floor(cpus * fraction)) if cpus else 0
    memory_fraction = min(float(prefs.get('ram_max_used_busy_pct', 50)),
                          float(prefs.get('ram_max_used_idle_pct', 90))) / 100
    memory = int(float(host.findtext('m_nbytes', '0')) * memory_fraction)
    disk = shutil.disk_usage(root).free
    disk = min(disk, int(float(prefs.get('disk_max_used_gb', 1000000)) * 1e9))
    disk = max(0, disk - int(float(prefs.get('disk_min_free_gb', .1)) * 1e9))
    gpus = []
    gpu_memory = {}
    for tag, vendor in [('coproc_cuda', 'nvidia'), ('coproc_intel_gpu', 'intel'), ('coproc_apple_gpu', 'apple')]:
        node = state.find('.//' + tag)
        if node is not None and float(node.findtext('count', '0')) > 0:
            gpus.append(vendor)
            gpu_memory[vendor] = max([float(n.text or 0) for tag in
                                     ('available_ram', 'totalGlobalMem', 'global_mem_size')
                                     for n in node.findall('.//' + tag)] or [0])
    runtime = False
    for name in ('docker', 'podman'):
        if shutil.which(name):
            result = subprocess.run([name, 'info'], stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL, timeout=15)
            runtime |= result.returncode == 0
    return dict(ready=True, project_url=url, host_id=int(project.findtext('hostid')),
                platform=state.findtext('platform_name'), cpus=usable,
                memory_bytes=memory, disk_bytes=disk, gpus=gpus, gpu_memory_bytes=gpu_memory,
                client_version=sum(int(state.findtext('core_client_' + key + '_version', '0')) * factor
                                   for key, factor in [('major', 10000), ('minor', 100), ('release', 1)]),
                container_runtime=runtime)


def main():
    action, url = sys.argv[1:]
    root = Path(os.environ.get('BOINC_DIR', '/var/lib/boinc'))
    if action == 'probe':
        try:
            receipt = probe(root, url)
        except (FileNotFoundError, ET.ParseError):
            receipt = {'ready': False}
        print(json.dumps(receipt))
    elif action == 'update':
        # Read the RPC password locally via boinccmd's normal data-directory lookup.
        subprocess.run(['boinccmd', '--project', url, 'update'], cwd=root, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
        print('{}')
    else:
        raise ValueError('unknown client action')


if __name__ == '__main__':
    main()
