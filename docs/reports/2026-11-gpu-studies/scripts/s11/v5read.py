#!/usr/bin/env python3
"""Read crops one at a time through llama-server with Study 9's region request, sending each image at native size
(as usnm_qwen request version 5 does). Stdlib only. --list JSONL {id,image}, --root DIR, --endpoint URL, --out JSONL."""
import argparse, base64, json, time, urllib.request
from pathlib import Path
ap = argparse.ArgumentParser()
ap.add_argument("--list", type=Path, required=True); ap.add_argument("--root", type=Path, required=True)
ap.add_argument("--endpoint", required=True); ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--max-tokens", type=int, default=2048)
a = ap.parse_args()
done = set()
if a.out.exists():
    done = {json.loads(l)["id"] for l in a.out.read_text().splitlines() if l.strip()}
with a.out.open("a") as out:
    for line in a.list.read_text().splitlines():
        if not line.strip(): continue
        r = json.loads(line)
        if r["id"] in done: continue
        p = a.root / r["image"]
        mime = "image/png" if p.suffix.lower() == ".png" else "image/jpeg"
        body = {"model": "xiaomi-ocr-0", "messages": [{"role": "user", "content": [
            {"type": "text", "text": "Extract the text in the image."},
            {"type": "image_url", "image_url": {"url": f"data:{mime};base64," + base64.b64encode(p.read_bytes()).decode()}}]}],
            "max_tokens": a.max_tokens, "temperature": 0.0, "top_p": 0.8, "presence_penalty": 0.0,
            "repetition_penalty": 1.0, "chat_template_kwargs": {"enable_thinking": False}}
        t = time.time()
        req = urllib.request.Request(a.endpoint + "/chat/completions", json.dumps(body).encode(), {"Content-Type": "application/json"})
        try:
            b = json.load(urllib.request.urlopen(req, timeout=900))
            c = b["choices"][0]; u = b.get("usage") or {}
            rec = {"id": r["id"], "text": (c["message"]["content"] or "").strip(), "finish_reason": c.get("finish_reason"),
                   "prompt_tokens": u.get("prompt_tokens"), "completion_tokens": u.get("completion_tokens"), "s": round(time.time() - t, 3)}
        except Exception as e:
            rec = {"id": r["id"], "text": None, "error": str(e)[:300], "s": round(time.time() - t, 3)}
        out.write(json.dumps(rec, ensure_ascii=False) + "\n"); out.flush()
