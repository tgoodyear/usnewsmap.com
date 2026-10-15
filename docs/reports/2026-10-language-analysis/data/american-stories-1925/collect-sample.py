#!/usr/bin/env python3
"""Read-only historical cohort/import-ID collection for #327. Run in the existing Azure ja-ocr image, with USNM_REFERENCE_URL and USNM_CURATED_URL; emits SHA-256-checked gzip/base64 console chunks. No store writes, leases, locks, or publication."""

import base64, collections, concurrent.futures, gzip, hashlib, io, json, os, sys, time
sys.path.insert(0, '/opt/jaocr')
import jaocr, quality
import pyarrow.parquet as pq
from azure.identity import DefaultAzureCredential
from azure.storage.blob import ContainerClient
VERSION='pages-v20261006-2'
class ReadOnly:
 def __init__(self,url): self.client=ContainerClient.from_container_url(url,credential=DefaultAzureCredential())
 def read(self,path): return self.client.download_blob(path).readall()
 def list(self,prefix): return [b.name for b in self.client.list_blobs(name_starts_with=prefix)]
r=ReadOnly(os.environ['USNM_REFERENCE_URL']); c=ReadOnly(os.environ['USNM_CURATED_URL'])
batches=json.loads(r.read(VERSION+'/batches.json')); titles={t['lccn']:t for t in json.loads(r.read(VERSION+'/titles.json'))}
for t in json.loads(r.read('catalog/titles.json')): titles.setdefault(t['lccn'],t)
english={l for l,t in titles.items() if (t.get('languages') or ['eng'])[0]=='eng'}
picked=[b for b in batches if (v:=b.get('curated') or {}).get('parts') and int(v.get('first','0')[:4] or 0)<=1925<=int(v.get('last','9999')[:4] or 9999)]
parts=[p for b in picked for p in b['curated']['parts']]
def loc(path):
 data=c.read(path); t=pq.read_table(io.BytesIO(data),columns=['doc_id','lccn','date','batch','page_key'])
 return [(x['doc_id'],{'lccn':x['lccn'],'date':str(x['date']),'batch':x['batch'],'page_key':x['page_key']}) for x in t.to_pylist() if x['date'].year==1925 and x['lccn'] in english and quality.sampled(x['doc_id'],1000)]
ours={}
with concurrent.futures.ThreadPoolExecutor(max_workers=4) as ex:
 for i,rows in enumerate(ex.map(loc,parts)):
  ours.update(rows)
  if i%50==0:print(json.dumps({'message':'issue327 progress','stage':'loc','parts':i,'pages':len(ours)}),flush=True)
paths=sorted(p for p in c.list('american-stories/pages/') if '/1925-' in p and p.endswith('.parquet'))
def ast(path):
 data=c.read(path); t=pq.read_table(io.BytesIO(data),columns=['doc_id']); return t.column('doc_id').to_pylist()
theirs=set()
with concurrent.futures.ThreadPoolExecutor(max_workers=4) as ex:
 for i,ids in enumerate(ex.map(ast,paths)):
  theirs.update(ids)
  if i%100==0:print(json.dumps({'message':'issue327 progress','stage':'american-stories','parts':i,'pages':len(theirs)}),flush=True)
for d,p in ours.items():p['imported']=d in theirs;p['title']=titles[p['lccn']]['name']
result={'version':VERSION,'sample_pct':10,'sample_pages':len(ours),'matched':sum(p['imported'] for p in ours.values()),'missing':sum(not p['imported'] for p in ours.values()),'imported_pages':len(theirs),'loc_parts':len(parts),'imported_parts':len(paths),'pages':ours,'imported_ids':sorted(theirs)}
raw=json.dumps(result,sort_keys=True,separators=(',',':')).encode(); encoded=base64.b64encode(gzip.compress(raw,mtime=0)).decode()
for i in range(0,len(encoded),4000): print(json.dumps({'message':'issue327 result','chunk':i//4000,'data':encoded[i:i+4000]}),flush=True)
print(json.dumps({'message':'issue327 finished','bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest(),'chunks':(len(encoded)+3999)//4000,**{k:v for k,v in result.items() if k not in ['pages','imported_ids']}}),flush=True)
