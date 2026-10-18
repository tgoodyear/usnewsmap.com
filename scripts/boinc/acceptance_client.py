"""Secret-free client probe, executed on an explicitly dedicated test client."""
import argparse
import json
import math
import os
import platform
import re
from pathlib import Path
import shutil
import subprocess
import xml.etree.ElementTree as ET


def directory_usage(root):
    used = 0
    def failed(error):
        raise error
    for directory, _names, files in os.walk(root, followlinks=False, onerror=failed):
        for name in files:
            path = Path(directory) / name
            try:
                if not path.is_symlink():
                    info = path.stat()
                    used += max(info.st_size, getattr(info, 'st_blocks', 0) * 512)
            except FileNotFoundError:
                # BOINC can retire a completed slot while preflight is reading it.
                continue
    return used


def disk_allowance(total, free, used, prefs):
    available = [free - float(prefs.get('disk_min_free_gb', .1)) * 1e9,
                 total * float(prefs.get('disk_max_used_pct', 90)) / 100 - used]
    absolute = float(prefs.get('disk_max_used_gb', 0))
    if absolute > 0:
        available.append(absolute * 1e9 - used)
    return max(0, int(min(available)))


def mac_podman():
    # Match BOINC's Terminal-PATH discovery without evaluating path_helper shell output.
    search_path = os.environ.get('PATH', '')
    try:
        result = subprocess.run(['/usr/libexec/path_helper', '-s'], capture_output=True,
                                text=True, timeout=5)
        match = re.search(r'(?:^|\n)PATH="([^"]*)"', result.stdout)
        if result.returncode == 0 and match:
            search_path = match[1]
    except (OSError, subprocess.TimeoutExpired):
        pass
    executable = shutil.which('podman', path=search_path)
    if executable:
        return executable
    homebrew = '/opt/homebrew/bin/podman' if platform.machine() == 'arm64' else '/usr/local/bin/podman'
    return next((p for p in ('/opt/podman/bin/podman', homebrew) if os.access(p, os.X_OK)), None)


def container_runtime():
    commands = []
    if platform.system() == 'Darwin':
        helper = '/Library/Application Support/BOINC Data/Run_Podman'
        executable = mac_podman() if os.access(helper, os.X_OK) else None
        if executable:
            commands.append(('podman', [helper, executable, 'info', '--format=json']))
    else:
        for name in ('docker', 'podman'):
            executable = shutil.which(name)
            if executable:
                args = ['--format', '{{json .}}'] if name == 'docker' else ['--format=json']
                commands.append((name, [executable, 'info', *args]))
    for name, command in commands:
        try:
            result = subprocess.run(command, capture_output=True, text=True, timeout=15)
            if result.returncode:
                continue
            info = json.loads(result.stdout)
            if not isinstance(info, dict):
                continue
            if name == 'docker' and isinstance(info.get('ServerVersion'), str) and info['ServerVersion']:
                return True
            if name == 'podman' and all(isinstance(info.get(k), dict) and info[k] for k in ('host', 'store')):
                return True
        except (OSError, subprocess.TimeoutExpired, ValueError):
            # Optional runtimes must neither reject native apps nor prevent fallback.
            continue
    return False


def boinccmd():
    executable = shutil.which('boinccmd')
    if executable:
        return executable
    if platform.system() == 'Darwin':
        executable = '/Applications/BOINCManager.app/Contents/Resources/boinccmd'
        if os.access(executable, os.X_OK):
            return executable
    raise FileNotFoundError('boinccmd is unavailable')


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
    cpu_pct = float(prefs.get('max_ncpus_pct', 100))
    fraction = min(cpu_pct or 100, 100) / 100
    usable = max(1, math.floor(cpus * fraction)) if cpus else 0
    memory_fraction = min(float(prefs.get('ram_max_used_busy_pct', 50)),
                          float(prefs.get('ram_max_used_idle_pct', 90))) / 100
    memory = int(float(host.findtext('m_nbytes', '0')) * memory_fraction)
    filesystem = shutil.disk_usage(root)
    disk = disk_allowance(filesystem.total, filesystem.free, directory_usage(root), prefs)
    gpus = []
    gpu_memory = {}
    gpu_capabilities = {}
    for tag, vendor in [('coproc_cuda', 'nvidia'), ('coproc_intel_gpu', 'intel'), ('coproc_apple_gpu', 'apple')]:
        node = state.find('.//' + tag)
        if node is not None and float(node.findtext('count', '0')) > 0:
            gpus.append(vendor)
            version = re.search(r'OpenCL\s+(\d+)\.(\d+)', node.findtext('.//opencl_device_version', ''))
            gpu_capabilities[vendor] = dict(cuda_version=int(node.findtext('.//cudaVersion', '0')),
                compute_capability=int(node.findtext('.//major', '0')) * 100 + int(node.findtext('.//minor', '0')),
                opencl_version=int(version[1]) * 100 + int(version[2]) if version else 0)
            gpu_memory[vendor] = max([float(n.text or 0) for tag in
                                     ('available_ram', 'totalGlobalMem', 'global_mem_size')
                                     for n in node.findall('.//' + tag)] or [0])
    return dict(ready=True, project_url=url, host_id=int(project.findtext('hostid')),
                platform=state.findtext('platform_name'), cpus=usable,
                memory_bytes=memory, disk_bytes=disk, gpus=gpus, gpu_memory_bytes=gpu_memory, gpu_capabilities=gpu_capabilities,
                client_version=sum(int(state.findtext(tag, '0')) * factor for tag, factor in
                                   [('core_client_major_version', 10000), ('core_client_minor_version', 100),
                                    ('core_client_release', 1)]),
                container_runtime=container_runtime())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--action", choices=["probe", "update"], required=True)
    parser.add_argument("--project-url", required=True)
    args = parser.parse_args()
    action, url = args.action, args.project_url
    default_root = '/Library/Application Support/BOINC Data' if platform.system() == 'Darwin' else '/var/lib/boinc'
    root = Path(os.environ.get('BOINC_DIR', default_root))
    if action == 'probe':
        try:
            receipt = probe(root, url)
        except (FileNotFoundError, ET.ParseError):
            receipt = {'ready': False}
        print(json.dumps(receipt))
    elif action == 'update':
        # Read the RPC password locally via boinccmd's normal data-directory lookup.
        subprocess.run([boinccmd(), '--project', url, 'update'], cwd=root, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
        print('{}')
    else:
        raise ValueError('unknown client action')


if __name__ == '__main__':
    main()
