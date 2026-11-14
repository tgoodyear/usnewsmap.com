"""Study 11: Qwen3-VL 8B through llama-server with usnm_qwen request version 3.

Version 3 (crates/usnm-ocr-contract/src/qwen.rs, crates/usnm-qwen-app/src/server.rs at origin/main): the PROMPT below
after the image, temperature 0, top_p 1.0, seed 42, the engine default image tokens, and the loop guard: the reply is
streamed and stopped when tail_repetition fires, checked every eight content deltas from the twelfth; the reading keeps
the text before the repeated run plus one period (loop_cut). Crops over 2,048 px on the long edge are fitted to 2,048
(Study 11 amendment). Rows are appended and flushed; ids already in --out are skipped.
"""
import argparse, base64, io, json, time, http.client
from pathlib import Path

PROMPT = ("Transcribe exactly the printed text in the image. Keep the original spelling, punctuation, line breaks and "
          "hyphenation. Do not correct or modernise anything, and do not add anything that is not printed. If a word is "
          "unreadable, write [?] for it. Output only the transcription.")
MAX_EDGE = 2048

ap = argparse.ArgumentParser()
ap.add_argument("--root", type=Path, required=True)
ap.add_argument("--list", type=Path, required=True)  # JSONL {id,image}
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--max-tokens", type=int, required=True)
ap.add_argument("--timeout", type=int, default=900)
a = ap.parse_args()
from PIL import Image


def tail_repetition(text):
    s = text.rstrip()
    n = len(s)
    for p in range(1, min(400, n // 3) + 1):
        i = n - 1
        while i >= p and s[i] == s[i - p]:
            i -= 1
        span = n - 1 - i + p
        if span >= 3 * p and span >= 30:
            return p, span
    return None


def loop_cut(text):
    t = text.rstrip()
    r = tail_repetition(t)
    if r is None:
        return None
    p, span = r
    return t[: len(t) - span + p]


def png_b64(path):
    with Image.open(path) as src:
        im = src.convert("RGB")
    fitted = max(im.size) > MAX_EDGE
    if fitted:
        im.thumbnail((MAX_EDGE, MAX_EDGE), Image.Resampling.LANCZOS)
    buf = io.BytesIO()
    im.save(buf, format="PNG")
    return base64.b64encode(buf.getvalue()).decode(), fitted


def read_one(b64):
    body = json.dumps({"messages": [{"role": "user", "content": [
        {"type": "image_url", "image_url": {"url": "data:image/png;base64," + b64}},
        {"type": "text", "text": PROMPT}]}],
        "max_tokens": a.max_tokens, "temperature": 0.0, "top_p": 1.0, "seed": 42, "stream": True,
        "stream_options": {"include_usage": True}})
    c = http.client.HTTPConnection("127.0.0.1", a.port, timeout=a.timeout)
    c.request("POST", "/v1/chat/completions", body=body, headers={"Content-Type": "application/json"})
    resp = c.getresponse()
    if resp.status != 200:
        err = resp.read()[-300:].decode("utf-8", "replace")
        c.close()
        return {"error": f"status {resp.status}: {err}"}
    raw, deltas, finish, stopped, usage = "", 0, None, False, {}
    while True:
        line = resp.readline()
        if not line:
            break
        line = line.decode("utf-8", "replace").strip()
        if not line.startswith("data:"):
            continue
        data = line[5:].strip()
        if data == "[DONE]":
            break
        v = json.loads(data)
        if "error" in v:
            c.close()
            return {"error": str(v["error"])[:300]}
        if v.get("usage"):
            usage = v["usage"]
        ch = (v.get("choices") or [{}])
        ch = ch[0] if ch else {}
        if ch.get("finish_reason"):
            finish = ch["finish_reason"]
        d = (ch.get("delta") or {}).get("content")
        if d:
            raw += d
            deltas += 1
            if deltas >= 12 and deltas % 8 == 0 and tail_repetition(raw) is not None:
                stopped = True
                break
    c.close()
    cut = loop_cut(raw) if stopped else None
    return {"text": (cut if cut is not None else raw).strip(), "raw_chars": len(raw), "finish_reason": "guard" if stopped else finish,
            "stopped": stopped, "deltas": deltas, "prompt_tokens": usage.get("prompt_tokens"), "completion_tokens": usage.get("completion_tokens")}


root = a.root.resolve()
done = set()
if a.out.exists():
    for l in a.out.read_text(encoding="utf-8").splitlines():
        if l.strip():
            r = json.loads(l)
            if "text" in r:
                done.add(r["id"])
with a.out.open("a", encoding="utf-8") as out:
    for l in a.list.read_text(encoding="utf-8").splitlines():
        if not l.strip():
            continue
        r = json.loads(l)
        if r["id"] in done:
            continue
        p = (root / r["image"]).resolve()
        p.relative_to(root)
        b64, fitted = png_b64(p)
        s = time.time()
        try:
            rec = read_one(b64)
        except Exception as e:  # recorded, not fatal
            rec = {"error": f"{type(e).__name__}: {e}"[:300]}
        rec.update({"id": r["id"], "fitted": fitted, "ms": int((time.time() - s) * 1000)})
        out.write(json.dumps(rec, ensure_ascii=False) + "\n")
        out.flush()
