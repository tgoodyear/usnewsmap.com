"""Synthetic regression tests; run from the repository root with unittest."""

import base64
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import time
import unittest


REPO = Path(__file__).resolve().parents[2]
HERE = REPO / "docs/reports/2026-11-gpu-studies/scripts"
LOCK = REPO / "ocr/engines/tesseract/models.lock"
SURVEY_LOCK = REPO / "scripts/ocr-survey/models.lock"
WORK = Path.cwd() / f".gpu-study-review-tests-{os.getpid()}"


def executable(path, text):
    path.write_text(text, encoding="utf-8")
    path.chmod(0o755)


def copied_script(source, destination, replacements=()):
    text = source.read_text(encoding="utf-8")
    for old, new in replacements:
        if text.count(old) != 1:
            raise AssertionError(f"expected exactly one {old!r} in {source}")
        text = text.replace(old, new)
    destination.write_text(text, encoding="utf-8")
    return destination


class StudyReviewTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        WORK.mkdir()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(WORK)

    def setUp(self):
        self.work = WORK / self._testMethodName
        self.work.mkdir()
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.trace = self.work / "trace.jsonl"
        self.env = os.environ.copy()
        self.env.update(
            PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
            TRACE=str(self.trace),
            PYTHONDONTWRITEBYTECODE="1",
        )
        executable(self.bin / "nice", '#!/usr/bin/env bash\nshift 2\nexec "$@"\n')
        executable(self.bin / "nproc", "#!/usr/bin/env bash\necho 3\n")
        executable(self.bin / "apt-get", "#!/usr/bin/env bash\nexit 0\n")
        executable(
            self.bin / "timeout",
            """#!/usr/bin/env bash
shift
for arg in "$@"; do
  case "$arg" in
    /work/in/*) case "${arg##*/}" in timeout*) if [ "${FAIL_READS:-0}" = 1 ]; then printf partial; exit 124; fi;; esac;;
  esac
done
exec "$@"
""",
        )
        executable(
            self.bin / "chroot",
            """#!/usr/bin/env python3
import json, os, pathlib, sys, time
args = sys.argv[1:]
if '--version' in args:
    event = {'event': 'version'}
else:
    image = next(a for a in args if a.startswith('/work/in/'))
    name = pathlib.Path(image).name.split('.')[0]
    event = {'event': 'read', 'id': name, 'psm': args[args.index('--psm') + 1],
             'lang': args[args.index('-l') + 1]}
with open(os.environ['TRACE'], 'a') as f: f.write(json.dumps(event) + '\\n')
if event['event'] == 'version':
    print('synthetic Tesseract')
elif name == 'fail' and os.environ.get('FAIL_READS') == '1':
    print('partial failed text')
    sys.exit(42)
else:
    if name == os.environ.get('BLOCK_ID'):
        pathlib.Path(os.environ['BLOCK_READY']).touch()
        while not pathlib.Path(os.environ['BLOCK_RELEASE']).exists(): time.sleep(0.02)
    if name != 'empty': print('synthetic ' + name)
""",
        )
        executable(
            self.bin / "base64",
            """#!/usr/bin/env python3
import base64, os, sys
sys.stdout.buffer.write(base64.b64encode(sys.stdin.buffer.read()))
sys.exit(9 if os.environ.get('FAIL_BASE64') == '1' else 0)
""",
        )
        executable(
            self.bin / "curl",
            """#!/usr/bin/env python3
import json, os, pathlib, sys, urllib.parse
args = sys.argv[1:]
url = args[-1]
if url == 'https://huggingface.co/api/models/Qwen/Qwen3-VL-8B-Instruct':
    if os.environ.get('API_FAIL') == '1': sys.exit(22)
    print(pathlib.Path(os.environ['API_BODY']).read_text(), end='')
    sys.exit(0)
parsed = urllib.parse.urlsplit(url)
if parsed.scheme != 'https' or parsed.netloc != 'fixture.invalid' or '-o' not in args:
    raise SystemExit('unexpected network request: ' + url)
model = pathlib.Path(parsed.path).stem
with open(os.environ['TRACE'], 'a') as f: f.write(json.dumps({'event': 'fetch', 'model': model}) + '\\n')
out = pathlib.Path(args[args.index('-o') + 1])
if model == os.environ.get('BAD_DOWNLOAD'):
    out.write_bytes(b'corrupt or incomplete synthetic model')
    sys.exit(28 if os.environ.get('DOWNLOAD_MODE') == 'partial' else 0)
out.write_bytes((pathlib.Path(os.environ['MODEL_REPLIES']) / model).read_bytes())
""",
        )

    def run_script(self, *args, env=None):
        return subprocess.run(
            args, cwd=self.work, env=env or self.env, text=True,
            capture_output=True, timeout=30, check=False,
        )

    def events(self):
        if not self.trace.exists():
            return []
        return [json.loads(line) for line in self.trace.read_text(encoding="utf-8").splitlines()]

    def models_fixture(self, base):
        base.mkdir(parents=True, exist_ok=True)
        models = {
            name: f"synthetic pinned model {name}\n".encode()
            for name in ("deu_latf", "frak2021", "eng", "deu", "fra", "fin", "swe", "osd", "survey_only")
        }
        replies = base / "model-replies"
        replies.mkdir()
        lines, survey_lines = [], []
        for name, body in models.items():
            (replies / name).write_bytes(body)
            line = (f"tessdata/{name}.traineddata https://fixture.invalid/{name}.traineddata "
                    f"{hashlib.sha256(body).hexdigest()}\n")
            (survey_lines if name in ("osd", "survey_only") else lines).append(line)
        lock = base / "models.lock"
        lock.write_text("".join(lines), encoding="utf-8")
        survey_lock = base / "survey-models.lock"
        survey_lock.write_text("".join(survey_lines), encoding="utf-8")
        helper = base / "tesseract-models.sh"
        text = (HERE / "tesseract-models.sh").read_text(encoding="utf-8")
        text, count = re.subn(
            r"(?m)^TESSERACT_MODEL_LOCK_SHA256=[0-9a-f]{64}$",
            f"TESSERACT_MODEL_LOCK_SHA256={hashlib.sha256(lock.read_bytes()).hexdigest()}",
            text,
        )
        self.assertEqual(count, 1)
        text, count = re.subn(
            r"(?m)^TESSERACT_SURVEY_LOCK_SHA256=[0-9a-f]{64}$",
            f"TESSERACT_SURVEY_LOCK_SHA256={hashlib.sha256(survey_lock.read_bytes()).hexdigest()}",
            text,
        )
        self.assertEqual(count, 1)
        helper.write_text(text, encoding="utf-8")
        env = self.env.copy()
        env.update(MODEL_REPLIES=str(replies), TESSERACT_SURVEY_MODELS_LOCK=str(survey_lock))
        return lock, helper, models, env

    def job_fixture(self, name, ids, lang="eng", psm="3"):
        base = self.work / name
        lock, helper, models, env = self.models_fixture(base)
        inp, out, rootfs = base / "in", base / "out", base / "rootfs"
        inp.mkdir()
        out.mkdir()
        image_models = rootfs / "opt/tessdata"
        image_models.mkdir(parents=True)
        for model, body in models.items():
            (image_models / f"{model}.traineddata").write_bytes(body)
        (image_models / "unknown.traineddata").write_bytes(b"not pinned")
        shutil.copy2(lock, inp / "models.lock")
        shutil.copy2(env["TESSERACT_SURVEY_MODELS_LOCK"], inp / "survey-models.lock")
        shutil.copy2(helper, inp / "tesseract-models.sh")
        executable(inp / "rootfs.sh", "#!/usr/bin/env bash\nexit 0\n")
        (inp / "reads.tsv").write_text(
            "".join(f"{item}\timages/{item}.png\n" for item in ids), encoding="utf-8"
        )
        (inp / "runs.tsv").write_text(f"reads.tsv\tbatch\t{lang}\t{psm}\n", encoding="utf-8")
        assignment = f"O={shlex.quote(str(out))}; I={shlex.quote(str(inp))}; R={shlex.quote(str(rootfs))}"
        job = copied_script(
            HERE / "jobs/tesseract.sh", base / "job.sh",
            [("O=/work/out; I=/work/in; R=/work/rootfs", assignment)],
        )
        return ("bash", str(job)), out, rootfs, image_models, models, env

    def local_fixture(self, name, ids, lang="eng", psm="3", absent=()):
        base = self.work / name
        lock, helper, models, env = self.models_fixture(base)
        scripts = base / "scripts"
        scripts.mkdir()
        shutil.copy2(helper, scripts / "tesseract-models.sh")
        script = shutil.copy2(HERE / "tesseract-local.sh", scripts)
        rootfs, images, out = base / "rootfs", base / "images", base / "out"
        tessdata = rootfs / "work/tessdata"
        tessdata.mkdir(parents=True)
        images.mkdir()
        for model, body in models.items():
            (tessdata / f"{model}.traineddata").write_bytes(body)
        for item in ids:
            if item not in absent:
                (images / f"{item}.png").write_bytes(b"synthetic image")
        listing = base / "reads.tsv"
        listing.write_text("".join(f"{item}\t{item}.png\n" for item in ids), encoding="utf-8")
        args = (
            "bash", str(script), "--list", str(listing), "--root", str(images), "--name", "batch",
            "--langs", lang, "--psm", psm, "--rootfs", str(rootfs), "--out", str(out),
            "--jobs", "3", "--models-lock", str(lock),
        )
        return args, out, rootfs, images, models, lock, env

    def decode(self, path):
        output = self.work / "decoded.jsonl"
        result = self.run_script("bash", str(HERE / "decode.sh"), "--kind", "plain", "--in", str(path), "--out", str(output))
        self.assertEqual(result.returncode, 0, result.stderr)
        return {row["id"]: row["text"] for line in output.read_text().splitlines() if (row := json.loads(line))}

    def test_archive_identifiers_are_literal_even_with_shell_metacharacters(self):
        archives, out = self.work / "archives", self.work / "alto"
        archives.mkdir()
        marker = self.work / "INJECTED"
        executable(self.bin / "id", '#!/usr/bin/env bash\ntouch "$INJECTION_MARKER"\n')
        env = self.env.copy()
        env.update(SCAN_ROOT=str(self.work), SCAN_MANIFESTS=str(self.work), LOC_ARCHIVES=str(archives),
                   INJECTION_MARKER=str(marker))
        batches = ["ordinary", "odd;id;", "odd batch';id;#"]
        rows = []
        for n, batch in enumerate(batches):
            doc = f"sn100{n}_1900-01-02_ed-1_seq-1"
            rows.append({"doc_id": doc, "batch": batch, "batch_version": 1, "iiif_base": "fixture:data:x"})
            member = f"sn100{n}/1900/01/02/ed-1/seq-1/ocr.xml"
            body = f"<alto>{n}</alto>".encode()
            with tarfile.open(archives / f"{batch}_ver01.tar.bz2", "w:bz2") as archive:
                entry = tarfile.TarInfo(member)
                entry.size = len(body)
                archive.addfile(entry, io.BytesIO(body))
        index, sample, bucket = self.work / "index.jsonl", self.work / "sample.csv", self.work / "bucket.tsv"
        index.write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")
        sample.write_text("".join(row["doc_id"] + ",fixture\n" for row in rows), encoding="utf-8")
        bucket.write_text("", encoding="utf-8")
        result = self.run_script(
            "bash", str(HERE / "fetch-alto.sh"), "--sample", str(sample), "--out", str(out),
            "--index", str(index), "--archives", str(archives), "--bucket", str(bucket), "--gap-seconds", "0",
            env=env,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(marker.exists())
        for n, row in enumerate(rows):
            self.assertEqual((out / f"{row['doc_id']}.xml").read_text(), f"<alto>{n}</alto>")
        self.assertEqual(len((out / "provenance.tsv").read_text().splitlines()), len(rows))

    def lora_fixture(self, name):
        base = self.work / name
        inp, out, modules = base / "in", base / "out", base / "modules"
        for path in (inp, out, modules):
            path.mkdir(parents=True)
        (self.bin / "python").symlink_to(sys.executable)
        (modules / "huggingface_hub.py").write_text(
            """import json, os
def snapshot_download(repo, revision, **kwargs):
    if os.environ.get('SNAPSHOT_FAIL') == '1': raise RuntimeError('synthetic download failure')
    with open(os.environ['TRACE'], 'a') as f:
        f.write(json.dumps({'event': 'download', 'repo': repo, 'revision': revision}) + '\\n')
    return 'synthetic snapshot'
""",
            encoding="utf-8",
        )
        body = """import json, os, pathlib, sys
with open(os.environ['TRACE'], 'a') as f:
    f.write(json.dumps({'event': 'worker', 'script': pathlib.Path(__file__).name, 'args': sys.argv[1:]}) + '\\n')
"""
        for name in ("lora_train.py", "lora_infer.py"):
            (inp / name).write_text(body, encoding="utf-8")
        (inp / "guard.txt").write_text("stop\n", encoding="utf-8")
        (inp / "selected.txt").write_text("adapter.safetensors\n", encoding="utf-8")
        (inp / "adapter.safetensors").write_bytes(b"synthetic adapter")
        api = base / "api.json"
        env = self.env.copy()
        env.update(PYTHONPATH=str(modules), API_BODY=str(api))
        replacement = f"O={shlex.quote(str(out))}; I={shlex.quote(str(inp))};"
        scripts = [
            copied_script(HERE / f"jobs/lora8b-{phase}.sh", base / f"phase-{phase}.sh",
                          [("O=/work/out; I=/work/in;", replacement)])
            for phase in (1, 2)
        ]
        return scripts, inp, out, api, env

    def test_hf_revision_is_validated_before_use_in_both_phases(self):
        scripts, inp, out, api, env = self.lora_fixture("lora")
        revision = "0123456789abcdef" * 2 + "01234567"
        bad = ["x');open('INJECTED','w').write('x');#", revision + "\n", "a" * 39, "A" * 40, "g" * 40]
        for value in bad:
            with self.subTest(api_revision=repr(value)):
                api.write_text(json.dumps({"sha": value}), encoding="utf-8")
                (out / "base-revision.txt").write_text("stale", encoding="utf-8")
                result = self.run_script("bash", str(scripts[0]), env=env)
                self.assertEqual(result.returncode, 6, result.stderr)
                self.assertFalse((out / "base-revision.txt").exists())
                self.assertFalse(self.events())
        self.assertFalse((self.work / "INJECTED").exists())

        api.write_text(f'{{"sha":"{revision}","sha":"invalid"}}', encoding="utf-8")
        result = self.run_script("bash", str(scripts[0]), env=env)
        self.assertEqual(result.returncode, 6, result.stderr)
        self.assertFalse((out / "base-revision.txt").exists())
        self.assertFalse(self.events())

        api.write_text(json.dumps({"sha": revision}), encoding="utf-8")
        (out / "base-revision.txt").write_text("stale", encoding="utf-8")
        env["API_FAIL"] = "1"
        result = self.run_script("bash", str(scripts[0]), env=env)
        self.assertEqual(result.returncode, 6, result.stderr)
        self.assertFalse((out / "base-revision.txt").exists())
        self.assertFalse(self.events())
        env.pop("API_FAIL")
        env["SNAPSHOT_FAIL"] = "1"
        result = self.run_script("bash", str(scripts[0]), env=env)
        self.assertEqual(result.returncode, 6, result.stderr)
        self.assertFalse((out / "base-revision.txt").exists())
        self.assertFalse(self.events())
        env.pop("SNAPSHOT_FAIL")
        result = self.run_script("bash", str(scripts[0]), env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((out / "base-revision.txt").read_text(), revision + "\n")
        self.assertEqual([e["revision"] for e in self.events() if e["event"] == "download"], [revision])
        train = next(e for e in self.events() if e.get("script") == "lora_train.py")
        self.assertEqual(train["args"][train["args"].index("--base-revision") + 1], revision)
        self.assertIn("--min-pixels", train["args"])

        self.trace.unlink()
        for value in (bad[0], revision + "\n\n"):
            with self.subTest(saved_revision=repr(value)):
                (inp / "base-revision.txt").write_text(value, encoding="utf-8")
                result = self.run_script("bash", str(scripts[1]), env=env)
                self.assertEqual(result.returncode, 6, result.stderr)
                self.assertFalse(self.events())
        (inp / "base-revision.txt").write_text(revision + "\n", encoding="utf-8")
        result = self.run_script("bash", str(scripts[1]), env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([e["revision"] for e in self.events() if e["event"] == "download"], [revision])
        self.assertFalse((self.work / "INJECTED").exists())

    def test_hf_revision_is_parsed_without_jq(self):
        scripts, _, _, api, env = self.lora_fixture("lora-no-jq")
        revision = "a" * 40
        api.write_text(f'{{"sha":"{revision}"}}', encoding="utf-8")
        for name in ("python3", "seq", "rm", "cat"):
            (self.bin / name).symlink_to(shutil.which(name))
        env["PATH"] = str(self.bin)
        result = self.run_script("/bin/bash", str(scripts[0]), env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([e["revision"] for e in self.events() if e["event"] == "download"], [revision])

    def test_export_treats_the_revision_and_settings_as_data(self):
        _, inp, out, _, env = self.lora_fixture("export")
        (self.bin / "python").unlink()
        executable(
            self.bin / "python",
            f'#!/usr/bin/env bash\nif [ "${{1:-}}" = -c ]; then exec {shlex.quote(sys.executable)} "$@"; fi\nexit 0\n',
        )
        executable(self.bin / "git", "#!/usr/bin/env bash\necho synthetic\n")
        executable(self.bin / "cmake", "#!/usr/bin/env bash\nexit 0\n")
        assignment = f"O={shlex.quote(str(out))}; I={shlex.quote(str(inp))}"
        script = copied_script(
            HERE / "jobs/export-gguf.sh", inp.parent / "export.sh",
            [("O=/work/out; I=/work/in", assignment),
             ('"$PY" "$I/merge_lora.py"', 'exit 0\n"$PY" "$I/merge_lora.py"')],
        )

        def settings(repo, revision):
            return (f"NAME=qwen8b-lora\nADAPTER=adapter.safetensors\nQUANT=Q4_K_M\n"
                    f"BASEREPO={repo}\nBASEREV={revision}\n")

        config = inp / "export.env"
        config.write_text(settings("Qwen/Qwen3-VL-8B-Instruct", "bad;touch INJECTED"), encoding="utf-8")
        result = self.run_script("bash", str(script), env=env)
        self.assertEqual(result.returncode, 6, result.stderr)
        self.assertFalse((self.work / "INJECTED").exists())
        self.assertFalse(self.events())

        revision = "a" * 40
        config.write_text(settings("Qwen/Qwen3-VL-8B-Instruct", revision), encoding="utf-8")
        result = self.run_script("bash", str(script), env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([e["revision"] for e in self.events() if e["event"] == "download"], [revision])

        self.trace.unlink()
        config.write_text(settings("", ""), encoding="utf-8")
        result = self.run_script("bash", str(script), env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.events())

    def test_export_driver_rejects_untrusted_revision_before_renting(self):
        root = self.work / "driver"
        scripts = root / "scripts/jobs"
        results = root / "results/s5-t8"
        scripts.mkdir(parents=True)
        results.mkdir(parents=True)
        (scripts / "merge_lora.py").write_text("synthetic", encoding="utf-8")
        (results / "adapter.safetensors").write_bytes(b"synthetic adapter")
        executable(root / "scripts/rent.sh", '#!/usr/bin/env bash\nprintf "rented\\n" > "$RENT_MARKER"\n')
        marker = self.work / "rented"
        env = self.env.copy()
        env.update(GPU_STUDY_ROOT=str(root), RENT_MARKER=str(marker))
        revision_file = results / "base-revision.txt"
        revision_file.write_text("bad;touch INJECTED\n", encoding="utf-8")
        args = ("bash", str(HERE / "study5.sh"), "--step", "export8", "--adapter", "adapter.safetensors")
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 6, result.stderr)
        self.assertFalse(marker.exists())
        self.assertFalse((root / "INJECTED").exists())

        revision = "b" * 40
        revision_file.write_text(revision + "\n", encoding="utf-8")
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(marker.exists())
        self.assertIn(f"BASEREV={revision}\n", (root / "push/s5-x8/export.env").read_text())

    def test_repository_lock_is_the_pinned_source(self):
        helper = (HERE / "tesseract-models.sh").read_text(encoding="utf-8")
        for variable, lock in (("TESSERACT_MODEL_LOCK_SHA256", LOCK),
                               ("TESSERACT_SURVEY_LOCK_SHA256", SURVEY_LOCK)):
            match = re.search(rf"(?m)^{variable}=([0-9a-f]{{64}})$", helper)
            self.assertIsNotNone(match)
            self.assertEqual(hashlib.sha256(lock.read_bytes()).hexdigest(), match.group(1))
        osd_line = next(line for line in SURVEY_LOCK.read_text().splitlines()
                        if line.startswith("tessdata/osd.traineddata "))
        _, url, checksum = osd_line.split()
        eng_line = next(line for line in LOCK.read_text().splitlines()
                        if line.startswith("tessdata/eng.traineddata "))
        self.assertEqual(url.rsplit("/", 1)[0], eng_line.split()[1].rsplit("/", 1)[0])
        result = self.run_script(
            "bash", "-c", '. "$1"; tesseract_verify_lock "$2" && tesseract_model_pin "$2" osd "$3"',
            "_", str(HERE / "tesseract-models.sh"), str(LOCK), str(SURVEY_LOCK),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"{url}\t{checksum}\n")
        unpinned = self.run_script(
            "bash", "-c", '. "$1"; tesseract_verify_lock "$2" && tesseract_model_pin "$2" jpn_vert "$3"',
            "_", str(HERE / "tesseract-models.sh"), str(LOCK), str(SURVEY_LOCK),
        )
        self.assertNotEqual(unpinned.returncode, 0)

    def test_study1_stages_the_model_lock_and_verifier(self):
        root = self.work / "study1"
        dev = root / "dev"
        scripts = root / "scripts/jobs"
        refs = root / "refs"
        images = root / "bench-v1.0.0/usnm-ocr-bench-v1.0.0/images"
        nzz = dev / "data/conv/nzz/images"
        crops, pages = dev / "pilot/work/crops", dev / "pilot/work/pages"
        for directory in (scripts, refs, images, nzz, crops, pages):
            directory.mkdir(parents=True)
        shutil.copy2(HERE / "tesseract-models.sh", scripts.parent / "tesseract-models.sh")
        executable(scripts / "rootfs.sh", "#!/usr/bin/env bash\nexit 0\n")
        shutil.copy2(HERE / "jobs/rootfs-layer.py", scripts / "rootfs-layer.py")
        marker = self.work / "rented"
        executable(root / "scripts/rent.sh", '#!/usr/bin/env bash\nprintf "rented\\n" > "$RENT_MARKER"\n')
        (images / "pub-reichsanzeiger-fixture.png").touch()
        (nzz / "fixture.png").touch()
        (crops / "fixture.png").touch()
        (pages / "page.jpg").touch()
        (refs / "rz-700.jsonl").write_text('{"id":"rz","image":"images/pub-reichsanzeiger-fixture.png"}\n')
        (refs / "nzz-360.jsonl").write_text('{"id":"nzz","image":"images/fixture.png"}\n')
        (crops / "crops.jsonl").write_text('{"id":"crop","file":"fixture.png"}\n')
        env = self.env.copy()
        env.update(GPU_STUDY_ROOT=str(root), GPU_STUDY_DEV=str(dev),
                   TESSERACT_MODELS_LOCK=str(LOCK), TESSERACT_SURVEY_MODELS_LOCK=str(SURVEY_LOCK),
                   RENT_MARKER=str(marker))
        result = self.run_script("bash", str(HERE / "study1.sh"), "--step", "tess", env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(marker.exists())
        staged = root / "push/s1-tess"
        self.assertEqual((staged / "models.lock").read_bytes(), LOCK.read_bytes())
        self.assertEqual((staged / "survey-models.lock").read_bytes(), SURVEY_LOCK.read_bytes())
        self.assertEqual((staged / "tesseract-models.sh").read_bytes(),
                         (HERE / "tesseract-models.sh").read_bytes())
        self.assertEqual((staged / "rootfs-layer.py").read_bytes(),
                         (HERE / "jobs/rootfs-layer.py").read_bytes())
        marker.unlink()
        bad_lock = root / "bad.lock"
        bad_lock.write_bytes(LOCK.read_bytes() + b"# altered\n")
        env["TESSERACT_MODELS_LOCK"] = str(bad_lock)
        result = self.run_script("bash", str(HERE / "study1.sh"), "--step", "tess", env=env)
        self.assertEqual(result.returncode, 6, result.stderr)
        self.assertFalse(marker.exists())
        env["TESSERACT_MODELS_LOCK"] = str(LOCK)
        bad_survey_lock = root / "bad-survey.lock"
        bad_survey_lock.write_bytes(SURVEY_LOCK.read_bytes() + b"# altered\n")
        env["TESSERACT_SURVEY_MODELS_LOCK"] = str(bad_survey_lock)
        result = self.run_script("bash", str(HERE / "study1.sh"), "--step", "tess", env=env)
        self.assertEqual(result.returncode, 6, result.stderr)
        self.assertFalse(marker.exists())
        env.pop("TESSERACT_SURVEY_MODELS_LOCK")
        result = self.run_script("bash", str(HERE / "study1.sh"), "--step", "tess", env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(marker.exists())
        self.assertFalse((staged / "survey-models.lock").exists())

    def test_rented_verifies_image_models_and_repairs_bad_cached_bytes(self):
        args, out, rootfs, image, models, env = self.job_fixture("repair", ["ok"], lang="fra+eng")
        (image / "frak2021.traineddata").write_bytes(b"wrong cached bytes")
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        tessdata = rootfs / "work/tessdata"
        self.assertEqual((tessdata / "frak2021.traineddata").read_bytes(), models["frak2021"])
        self.assertEqual((tessdata / "fra.traineddata").read_bytes(), models["fra"])
        self.assertFalse((tessdata / "unknown.traineddata").exists())
        self.assertEqual([e["model"] for e in self.events() if e["event"] == "fetch"], ["frak2021"])
        self.assertEqual(len((out / "tessdata.sha256").read_text().splitlines()), 5)
        self.assertEqual(self.decode(out / "batch.b64"), {"ok": "synthetic ok\n"})

    def test_local_osd_modes_use_the_verified_survey_pin(self):
        for psm in ("0", "1", "12", "01"):
            with self.subTest(psm=psm):
                self.trace.unlink(missing_ok=True)
                args, out, _, _, _, _, env = self.local_fixture(f"osd-local-{psm}", ["ok"], psm=psm)
                result = self.run_script(*args, env=env)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.decode(out / "batch.b64"), {"ok": "synthetic ok\n"})
                reads = [e for e in self.events() if e["event"] == "read"]
                self.assertEqual([(e["id"], e["psm"]) for e in reads], [("ok", psm)])

        args, out, _, _, _, _, env = self.local_fixture("osd-explicit", ["ok"], lang="eng+osd", psm="3")
        survey_lock = env["TESSERACT_SURVEY_MODELS_LOCK"]
        env["TESSERACT_SURVEY_MODELS_LOCK"] = str(self.work / "absent.lock")
        result = self.run_script(*args, "--survey-models-lock", survey_lock, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.decode(out / "batch.b64"), {"ok": "synthetic ok\n"})

        for location in ("repository", "study-root"):
            with self.subTest(survey_lookup=location):
                args, out, _, _, _, _, env = self.local_fixture(f"osd-lookup-{location}", ["ok"], psm="1")
                survey_lock = Path(env.pop("TESSERACT_SURVEY_MODELS_LOCK"))
                base = Path(args[1]).parent.parent
                if location == "repository":
                    repo = base / "fake-repo"
                    scripts = repo / "docs/reports/2026-11-gpu-studies/scripts"
                    scripts.mkdir(parents=True)
                    for name in ("tesseract-local.sh", "tesseract-models.sh"):
                        shutil.copy2(Path(args[1]).parent / name, scripts / name)
                    args = (args[0], str(scripts / "tesseract-local.sh"), *args[2:])
                    env.pop("GPU_STUDY_ROOT", None)
                else:
                    scripts = base / "scripts/copied"
                    scripts.mkdir()
                    for name in ("tesseract-local.sh", "tesseract-models.sh"):
                        shutil.copy2(Path(args[1]).parent / name, scripts / name)
                    args = (args[0], str(scripts / "tesseract-local.sh"), *args[2:])
                    repo = base / "repo"
                    env["GPU_STUDY_ROOT"] = str(base)
                destination = repo / "scripts/ocr-survey/models.lock"
                destination.parent.mkdir(parents=True)
                shutil.copy2(survey_lock, destination)
                result = self.run_script(*args, env=env)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.decode(out / "batch.b64"), {"ok": "synthetic ok\n"})

    def test_local_osd_fails_closed_on_missing_or_changed_inputs(self):
        for setting in ("missing-lock", "bad-lock", "missing-model", "bad-model", "symlink-model"):
            with self.subTest(setting=setting):
                self.trace.unlink(missing_ok=True)
                args, out, rootfs, _, models, _, env = self.local_fixture(f"osd-{setting}", ["ok"], psm="1")
                survey_lock = Path(env["TESSERACT_SURVEY_MODELS_LOCK"])
                model = rootfs / "work/tessdata/osd.traineddata"
                if setting == "missing-lock":
                    survey_lock.unlink()
                elif setting == "bad-lock":
                    with survey_lock.open("a", encoding="utf-8") as f:
                        f.write("# changed\n")
                elif setting == "missing-model":
                    model.unlink()
                elif setting == "bad-model":
                    model.write_bytes(b"wrong osd")
                else:
                    external = self.work / "osd-symlink-target"
                    external.write_bytes(models["osd"])
                    model.unlink()
                    model.symlink_to(external)
                result = self.run_script(*args, env=env)
                self.assertEqual(result.returncode, 6, result.stderr)
                self.assertFalse(any(e["event"] == "read" for e in self.events()))
                self.assertFalse((out / "batch.b64").exists())

    def test_rented_osd_modes_verify_and_record_the_survey_pin(self):
        for psm, lang in (("0", "eng"), ("1", "eng"), ("12", "eng"), ("3", "eng+osd")):
            with self.subTest(psm=psm, lang=lang):
                self.trace.unlink(missing_ok=True)
                args, out, rootfs, _, models, env = self.job_fixture(f"osd-job-{psm}", ["ok"], lang=lang, psm=psm)
                result = self.run_script(*args, env=env)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.decode(out / "batch.b64"), {"ok": "synthetic ok\n"})
                self.assertEqual((rootfs / "work/tessdata/osd.traineddata").read_bytes(), models["osd"])
                receipt = (out / "tessdata.sha256").read_text().splitlines()
                self.assertEqual(sum(line.endswith("  osd.traineddata") for line in receipt), 1)
                self.assertIn(f"{hashlib.sha256(models['osd']).hexdigest()}  osd.traineddata", receipt)
                self.assertFalse(any(e["event"] == "fetch" for e in self.events()))
                reads = [e for e in self.events() if e["event"] == "read"]
                self.assertEqual([(e["psm"], e["lang"]) for e in reads], [(psm, lang)])

    def test_rented_osd_rejects_bad_locks_and_bad_downloads(self):
        for setting in ("missing-lock", "bad-lock", "corrupt", "partial"):
            with self.subTest(setting=setting):
                self.trace.unlink(missing_ok=True)
                args, out, rootfs, image, _, env = self.job_fixture(f"osd-job-{setting}", ["ok"], psm="1")
                survey_lock = Path(args[1]).parent / "in/survey-models.lock"
                if setting == "missing-lock":
                    survey_lock.unlink()
                elif setting == "bad-lock":
                    with survey_lock.open("a", encoding="utf-8") as f:
                        f.write("# changed\n")
                else:
                    (image / "osd.traineddata").unlink()
                    env.update(BAD_DOWNLOAD="osd", DOWNLOAD_MODE=setting)
                result = self.run_script(*args, env=env)
                self.assertEqual(result.returncode, 6, result.stderr)
                self.assertFalse(any(e["event"] in ("read", "version") for e in self.events()))
                self.assertFalse((out / "batch.b64").exists())
                self.assertFalse((rootfs / "work/tessdata/osd.traineddata").exists())
                if setting in ("missing-lock", "bad-lock"):
                    self.assertFalse(self.events())

        self.trace.unlink(missing_ok=True)
        args, out, rootfs, image, models, env = self.job_fixture("osd-job-repair", ["ok"], psm="12")
        (image / "osd.traineddata").write_bytes(b"wrong cached osd")
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((rootfs / "work/tessdata/osd.traineddata").read_bytes(), models["osd"])
        self.assertEqual([e["model"] for e in self.events() if e["event"] == "fetch"], ["osd"])
        self.assertEqual(self.decode(out / "batch.b64"), {"ok": "synthetic ok\n"})

    def test_non_osd_runs_do_not_require_a_survey_lock(self):
        args, out, _, _, _, _, env = self.local_fixture("without-survey-local", ["ok"])
        env["TESSERACT_SURVEY_MODELS_LOCK"] = str(self.work / "absent-survey.lock")
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.decode(out / "batch.b64"), {"ok": "synthetic ok\n"})

        self.trace.unlink(missing_ok=True)
        args, out, _, image, _, env = self.job_fixture("without-survey-job", ["ok"])
        (Path(args[1]).parent / "in/survey-models.lock").unlink()
        (image / "osd.traineddata").unlink()
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.decode(out / "batch.b64"), {"ok": "synthetic ok\n"})
        self.assertNotIn("osd.traineddata", (out / "tessdata.sha256").read_text())
        self.assertFalse(any(e["event"] == "fetch" for e in self.events()))

    def test_corrupt_or_interrupted_model_download_never_reaches_tesseract(self):
        for mode in ("corrupt", "partial"):
            with self.subTest(mode=mode):
                self.trace.unlink(missing_ok=True)
                args, out, rootfs, image, _, env = self.job_fixture(mode, ["ok"])
                (image / "frak2021.traineddata").unlink()
                env.update(BAD_DOWNLOAD="frak2021", DOWNLOAD_MODE=mode)
                result = self.run_script(*args, env=env)
                self.assertEqual(result.returncode, 6, result.stderr)
                self.assertFalse((rootfs / "work/tessdata/frak2021.traineddata").exists())
                self.assertEqual(list((rootfs / "work/tessdata").glob("*.part*")), [])
                self.assertFalse(any(e["event"] in ("read", "version") for e in self.events()))
                self.assertFalse((out / "batch.b64").exists())

    def test_model_symlinks_cannot_disagree_with_the_chroot(self):
        for kind in ("file", "directory", "work"):
            with self.subTest(kind=kind):
                self.trace.unlink(missing_ok=True)
                args, _, rootfs, _, models, _, env = self.local_fixture(f"link-{kind}", ["ok"])
                path = rootfs / "work/tessdata/eng.traineddata"
                if kind == "file":
                    target = self.work / "outside-model"
                    target.write_bytes(models["eng"])
                    path.unlink()
                elif kind == "directory":
                    path = path.parent
                    target = self.work / "outside-tessdata"
                    path.rename(target)
                else:
                    path = rootfs / "work"
                    target = self.work / "outside-work"
                    path.rename(target)
                path.symlink_to(target)
                result = self.run_script(*args, env=env)
                self.assertEqual(result.returncode, 6, result.stderr)
                self.assertFalse(any(e["event"] == "read" for e in self.events()))

        for kind in ("existing", "dangling"):
            with self.subTest(rented_link=kind):
                self.trace.unlink(missing_ok=True)
                args, out, rootfs, _, models, env = self.job_fixture(f"link-job-{kind}", ["ok"])
                tessdata = rootfs / "work/tessdata"
                tessdata.mkdir(parents=True)
                outside = self.work / f"outside-rented-{kind}"
                if kind == "existing":
                    outside.write_bytes(models["eng"])
                (tessdata / "eng.traineddata").symlink_to(outside)
                result = self.run_script(*args, env=env)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse((tessdata / "eng.traineddata").is_symlink())
                if kind == "existing":
                    self.assertEqual(outside.read_bytes(), models["eng"])
                else:
                    self.assertFalse(outside.exists())
                self.assertEqual([e["model"] for e in self.events() if e["event"] == "fetch"], ["eng"])
                self.assertEqual(self.decode(out / "batch.b64"), {"ok": "synthetic ok\n"})

        self.trace.unlink(missing_ok=True)
        args, _, rootfs, _, _, env = self.job_fixture("linked-job-directory", ["ok"])
        outside = self.work / "outside-job-directory"
        outside.mkdir()
        (rootfs / "work").mkdir()
        (rootfs / "work/tessdata").symlink_to(outside)
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 6, result.stderr)
        self.assertFalse(self.events())
        self.assertEqual(list(outside.iterdir()), [])

    def test_unknown_models_and_bad_worker_locks_are_rejected(self):
        for setting in ("unknown", "survey-only", "bad-lock", "bad-bytes"):
            with self.subTest(setting=setting):
                self.trace.unlink(missing_ok=True)
                lang = {"unknown": "eng+jpn_vert", "survey-only": "survey_only"}.get(setting, "eng")
                args, out, rootfs, _, _, lock, env = self.local_fixture(setting, ["ok"], lang=lang)
                (rootfs / "work/tessdata/jpn_vert.traineddata").write_bytes(b"unknown model")
                if setting == "bad-lock":
                    with lock.open("a", encoding="utf-8") as f:
                        f.write("# changed\n")
                if setting == "bad-bytes":
                    (rootfs / "work/tessdata/eng.traineddata").write_bytes(b"substituted")
                result = self.run_script(*args, env=env)
                self.assertEqual(result.returncode, 6, result.stderr)
                self.assertFalse(any(e["event"] == "read" for e in self.events()))
                self.assertFalse((out / "batch.b64").exists())

        self.trace.unlink(missing_ok=True)
        args, _, _, _, _, env = self.job_fixture("unknown-job", ["ok"], lang="jpn_vert")
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 6, result.stderr)
        self.assertFalse(self.events())

    def test_local_failures_are_missing_and_retries_replace_stale_rows(self):
        ids = ["ok", "empty", "fail", "timeout", "missing"]
        args, out, _, images, _, _, env = self.local_fixture("local", ids, absent=("missing",))
        out.mkdir()
        stale = base64.b64encode(b"old reading").decode()
        (out / "batch.b64").write_text(f"prior\t{stale}\nfail\t{stale}\n", encoding="utf-8")
        (out / "batch.failures.tsv").write_text("prior-failure\t17\nfail\t88\n", encoding="utf-8")
        env["FAIL_READS"] = "1"
        result = self.run_script(*args, env=env)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.decode(out / "batch.b64"), {"prior": "old reading", "ok": "synthetic ok\n", "empty": ""})
        failures = dict(line.split("\t") for line in (out / "batch.failures.tsv").read_text().splitlines())
        self.assertEqual(failures["fail"], "42")
        self.assertEqual(failures["timeout"], "124")
        self.assertNotEqual(failures["missing"], "0")
        self.assertEqual(failures["prior-failure"], "17")
        self.assertEqual(len(failures), 4)
        self.assertIn("failed batch", (out / "progress.txt").read_text())
        self.assertNotIn("done batch", (out / "progress.txt").read_text())
        self.assertEqual(list((images.parent / "rootfs/work/in").iterdir()), [])

        (images / "missing.png").write_bytes(b"synthetic image")
        env["FAIL_READS"] = "0"
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = self.decode(out / "batch.b64")
        self.assertEqual(set(rows), {"prior", *ids})
        self.assertEqual(rows["fail"], "synthetic fail\n")
        self.assertEqual(rows["timeout"], "synthetic timeout\n")
        self.assertEqual(len((out / "batch.b64").read_text().splitlines()), len(rows))
        self.assertEqual((out / "batch.failures.tsv").read_text(), "prior-failure\t17\n")

    def test_rented_failures_are_missing_and_not_marked_done(self):
        args, out, _, _, _, env = self.job_fixture("failed-job", ["ok", "empty", "fail", "timeout"])
        env["FAIL_READS"] = "1"
        result = self.run_script(*args, env=env)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.decode(out / "batch.b64"), {"ok": "synthetic ok\n", "empty": ""})
        failures = dict(line.split("\t") for line in (out / "batch.failures.tsv").read_text().splitlines())
        self.assertEqual(failures, {"fail": "42", "timeout": "124"})
        self.assertNotIn("done batch", (out / "progress.txt").read_text())

    def test_rented_failure_does_not_skip_later_runs(self):
        args, out, _, _, _, env = self.job_fixture("multiple-runs", ["fail"])
        inp = Path(args[1]).parent / "in"
        (inp / "second.tsv").write_text("ok\timages/ok.png\n", encoding="utf-8")
        (inp / "runs.tsv").write_text("reads.tsv\tbatch\teng\t3\nsecond.tsv\tother\teng\t3\n")
        env["FAIL_READS"] = "1"
        result = self.run_script(*args, env=env)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.decode(out / "batch.b64"), {})
        self.assertEqual(self.decode(out / "other.b64"), {"ok": "synthetic ok\n"})
        self.assertIn("failed batch", (out / "progress.txt").read_text())
        self.assertIn("done other", (out / "progress.txt").read_text())

    def test_rented_rejects_duplicate_run_names(self):
        args, _, _, _, _, env = self.job_fixture("duplicate-runs", ["ok"])
        inp = Path(args[1]).parent / "in"
        (inp / "runs.tsv").write_text("reads.tsv\tbatch\teng\t3\nreads.tsv\tbatch\teng\t6\n")
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 6, result.stderr)
        self.assertFalse(self.events())

    def test_rented_completed_parts_remain_fetchable_during_a_run(self):
        args, out, _, _, _, env = self.job_fixture("live-parts", ["ok", "hold"])
        ready, release = self.work / "ready", self.work / "release"
        env.update(BLOCK_ID="hold", BLOCK_READY=str(ready), BLOCK_RELEASE=str(release))
        process = subprocess.Popen(args, cwd=self.work, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            end = time.monotonic() + 10
            parts = []
            while time.monotonic() < end and process.poll() is None:
                parts = list(out.glob("batch.*.part"))
                if ready.exists() and parts:
                    break
                time.sleep(0.05)
            self.assertTrue(ready.exists())
            self.assertEqual(len(parts), 1)
            self.assertTrue(parts[0].read_text().startswith("ok\t"))
            self.assertFalse((out / "batch.b64").exists())
            release.touch()
            _, stderr = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0, stderr.decode())
            self.assertEqual(set(self.decode(out / "batch.b64")), {"ok", "hold"})
            self.assertEqual(list(out.glob("batch.*.part")), [])
        finally:
            release.touch()
            if process.poll() is None:
                process.terminate()
                process.communicate(timeout=10)

    def test_local_completed_parts_survive_while_a_run_is_active(self):
        args, out, _, _, _, _, env = self.local_fixture("local-live-parts", ["ok", "hold"])
        ready, release = self.work / "local-ready", self.work / "local-release"
        env.update(BLOCK_ID="hold", BLOCK_READY=str(ready), BLOCK_RELEASE=str(release))
        process = subprocess.Popen(args, cwd=self.work, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            end = time.monotonic() + 10
            parts = []
            while time.monotonic() < end and process.poll() is None:
                parts = list(out.glob("batch.*.part"))
                if ready.exists() and parts:
                    break
                time.sleep(0.05)
            self.assertTrue(ready.exists())
            self.assertEqual(len(parts), 1)
            self.assertTrue(parts[0].read_text().startswith("ok\t"))
            release.touch()
            _, stderr = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0, stderr.decode())
            self.assertEqual(set(self.decode(out / "batch.b64")), {"ok", "hold"})
            self.assertEqual(list(out.glob("batch.*.part")), [])
        finally:
            release.touch()
            if process.poll() is None:
                process.terminate()
                process.communicate(timeout=10)

    def test_local_retry_does_not_merge_an_interrupted_runs_parts(self):
        args, out, _, _, _, _, env = self.local_fixture("stale-parts", ["ok"])
        out.mkdir()
        pidfile, start = self.work / "pid", self.work / "start"
        wrapper = self.work / "wait.sh"
        executable(
            wrapper,
            '#!/usr/bin/env bash\nprintf "%s" "$$" > "$PID_FILE"\n'
            'while [ ! -e "$START_FILE" ]; do sleep 0.02; done\nexec "$@"\n',
        )
        env.update(PID_FILE=str(pidfile), START_FILE=str(start))
        process = subprocess.Popen(
            ["/bin/bash", str(wrapper), *args], cwd=self.work, env=env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        try:
            end = time.monotonic() + 10
            while time.monotonic() < end and not pidfile.exists():
                time.sleep(0.05)
            self.assertTrue(pidfile.exists())
            stale = out / f"batch.{pidfile.read_text()}.0.123.part"
            stale.write_text(f"stale\t{base64.b64encode(b'old').decode()}\n")
            start.touch()
            _, stderr = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0, stderr.decode())
            self.assertEqual(self.decode(out / "batch.b64"), {"ok": "synthetic ok\n"})
            self.assertTrue(stale.exists())
        finally:
            start.touch()
            if process.poll() is None:
                process.terminate()
                process.communicate(timeout=10)

    def test_encoder_failures_are_missing_and_empty_files_decode(self):
        for kind in ("local", "job"):
            with self.subTest(kind=kind):
                self.trace.unlink(missing_ok=True)
                if kind == "local":
                    args, out, _, _, _, _, env = self.local_fixture("encoder-local", ["ok"])
                else:
                    args, out, _, _, _, env = self.job_fixture("encoder-job", ["ok"])
                env["FAIL_BASE64"] = "1"
                result = self.run_script(*args, env=env)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.decode(out / "batch.b64"), {})
                self.assertEqual((out / "batch.failures.tsv").read_text(), "ok\t9\n")
        missing = self.run_script(
            "bash", str(HERE / "decode.sh"), "--kind", "plain", "--in", str(self.work / "absent"),
            "--out", str(self.work / "missing.jsonl"),
        )
        self.assertNotEqual(missing.returncode, 0)


if __name__ == "__main__":
    unittest.main()
