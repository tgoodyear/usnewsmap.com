#!/usr/bin/env python3
"""Read-only upstream scan-name collection for #327. Run in the existing Azure ja-ocr image; streams the 1.4 GB archive once, discards OCR text, and emits SHA-256-checked gzip/base64 console chunks. No store writes."""

import base64,collections,gzip,hashlib,json,sys
sys.path.insert(0,'/opt/jaocr')
import american_stories as ams, quality
ams.URL = 'https://huggingface.co/datasets/dell-research-harvard/AmericanStories/resolve/77e27fa69c4788dfaad1c9efd8a226d5a32d3e9a/faro_{year}.tar.gz'
names=[]; batches=collections.Counter(); examples={}; selected={}
with ams.tar_stream(1925) as tar:
 for member in tar:
  if not(member.isfile() and member.name.endswith('.json')):continue
  names.append(member.name)
  # Read each JSON once to group upstream scans by their original batch; retain no OCR text.
  scan=json.load(tar.extractfile(member)); loc=(scan.get('scan') or {}).get('raw_data_loc')
  if loc:
   m=ams.BATCH.search(loc); b=m.group(1) if m else 'unknown';batches[b]+=1;examples.setdefault(b,loc)
  got=ams.parse(member.name)
  if got:
   l,d,e,i,p=got
   if p is None or quality.sampled(f'{l}_{d}_ed-{e}_seq-{p}',1000): selected[member.name]=loc
  if len(names)%10000==0: print(json.dumps({'message':'issue327 upstream progress','scans':len(names)}),flush=True)
ids,stats=ams.place(names)
result={'names':names,'batch_counts':dict(batches),'batch_examples':examples,'sample_scan_urls':selected,'placement':stats,'unique_pages':len(ids)}
raw=json.dumps(result,sort_keys=True,separators=(',',':')).encode();encoded=base64.b64encode(gzip.compress(raw,mtime=0)).decode()
for i in range(0,len(encoded),4000):print(json.dumps({'message':'issue327 upstream result','chunk':i//4000,'data':encoded[i:i+4000]}),flush=True)
print(json.dumps({'message':'issue327 upstream finished','bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest(),'chunks':(len(encoded)+3999)//4000,'scans':len(names),'unique_pages':len(ids),'placement':stats}),flush=True)
