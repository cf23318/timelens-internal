from contextlib import contextmanager
from dataclasses import replace
import hashlib,io,json,sqlite3
from uuid import uuid4
from fastapi.testclient import TestClient
from PIL import Image
import pytest
from timelens_upload.config import Settings
from timelens_upload.app import create_app
from fakes import FakeBucket
TOKEN="fixture-only-device-token-not-a-real-secret-123456"
def image_bytes():
 out=io.BytesIO();Image.new("RGB",(16,16),(30,140,210)).save(out,format="WEBP",quality=85);return out.getvalue()
def headers(data,**changes):
 h={"Authorization":"Bearer "+TOKEN,"Content-Type":"image/webp","X-TimeLens-Survey":"fixture-survey","X-TimeLens-Device":"fixture-device","X-TimeLens-Captured-At":"2026-10-01T01:02:03.123Z","X-TimeLens-Sha256":hashlib.sha256(data).hexdigest(),"X-TimeLens-Size-Bytes":str(len(data)),"X-TimeLens-Capture-Group-ID":str(uuid4()),"X-TimeLens-Group-Elapsed-Ms":"0","X-TimeLens-Expected-Displays":"1","X-TimeLens-Display-ID":"1"};h.update(changes);return h
@pytest.fixture
def env(tmp_path):
 p=tmp_path/"tokens.json";p.write_text(json.dumps({"fixture-survey":{"fixture-device":TOKEN}}));p.chmod(0o600)
 s=Settings(tmp_path/"persistent",p,"fixture-private-bucket",min_free_bytes=0);b=FakeBucket();return s,b,TestClient(create_app(s,b))
def send(c,data=None,h=None,sid=None):
 data=image_bytes() if data is None else data
 return c.post("/v1/frames/"+(sid or str(uuid4())),content=data,headers=h or headers(data))
def test_idempotent_retry_and_restart(env):
 s,b,c=env;d=image_bytes();h=headers(d);sid=str(uuid4());r=send(c,d,h,sid);assert r.status_code==200,r.text
 assert set(r.json())=={"snapshot_id","object_key","sha256","size_bytes","verified"};assert r.json()["verified"] is True
 assert send(c,d,h,sid).json()==r.json();assert send(TestClient(create_app(s,b)),d,h,sid).json()==r.json();assert b.put_count==1
 assert not list((s.spool_dir/"pending").iterdir());assert (s.spool_dir/"receipts.sqlite3").stat().st_mode & 0o077==0
def test_single_and_multiple_displays_without_duration_gate(env):
 s,b,c=env;d=image_bytes();g=str(uuid4())
 for display in (1,9):assert send(c,d,headers(d,**{"X-TimeLens-Capture-Group-ID":g,"X-TimeLens-Expected-Displays":"2","X-TimeLens-Display-ID":str(display)})).status_code==200
 assert send(c).status_code==200;assert c.get("/healthz").json()["automatic_retention"] is False;assert not list(s.spool_dir.glob("*deadline*"))
@pytest.mark.parametrize("change",[{"X-TimeLens-Captured-At":"2026-10-02T01:02:03.123Z"},{"X-TimeLens-Display-ID":"2"},{"X-TimeLens-Capture-Group-ID":str(uuid4())}])
def test_uuid_identity_cannot_change_even_across_dates(env,change):
 _,b,c=env;d=image_bytes();h=headers(d);sid=str(uuid4());assert send(c,d,h,sid).status_code==200;assert send(c,d,{**h,**change},sid).status_code==409;assert b.put_count==1
def test_group_display_is_unique(env):
 _,b,c=env;d=image_bytes();h=headers(d);assert send(c,d,h).status_code==200;assert send(c,d,h).status_code==409;assert b.put_count==1
@pytest.mark.parametrize("field,value",[("X-TimeLens-Sha256","0"*64),("X-TimeLens-Size-Bytes","999"),("X-TimeLens-Group-Elapsed-Ms","15001"),("X-TimeLens-Expected-Displays","0"),("X-TimeLens-Display-ID","-1"),("X-TimeLens-Survey","../escape"),("X-TimeLens-Captured-At","2026-10-01"),("X-TimeLens-Capture-Group-ID","bad"),("Content-Type","text/plain")])
def test_invalid_headers_never_reach_oss(env,field,value):
 s,b,c=env;d=image_bytes();r=send(c,d,headers(d,**{field:value}));assert r.status_code in (400,401,413,415);assert b.put_count==0;assert not list((s.spool_dir/"pending").iterdir())
@pytest.mark.parametrize("field",["private","object_private","public_block","policy","versioning","bad_crc"])
def test_private_storage_verification(env,field):
 _,b,c=env;setattr(b,field,"Enabled" if field=="versioning" else True if field in {"policy","bad_crc"} else False);r=send(c);assert r.status_code in (409,503);assert "verified" not in r.json()
def test_decode_invalid_webp(env):
 _,b,c=env;d=b"RIFF"+(12).to_bytes(4,"little")+b"WEBPnotimage";assert send(c,d).status_code==422;assert b.put_count==0
def test_wrong_device_and_live_revocation(env):
 s,b,c=env;d=image_bytes();h=headers(d);assert send(c,d,{**h,"X-TimeLens-Device":"other"}).status_code==401;assert send(c,d,{**h,"Authorization":"Bearer wrong"}).status_code==401
 s.tokens_file.write_text("{}");assert send(c,d,h).status_code==401;assert b.put_count==0
def test_revocation_during_put_never_returns_receipt(env):
 s,b,c=env;b.after_put=lambda:s.tokens_file.write_text("{}");assert send(c).status_code==401
 with sqlite3.connect(s.spool_dir/"receipts.sqlite3") as db:assert db.execute("SELECT confirmed FROM frames").fetchone()==(0,)
def test_lost_receipt_can_retry_without_second_put(env,monkeypatch):
 import timelens_upload.app as m
 s,b,c=env;original=m.registry;calls=0
 @contextmanager
 def failing(config):
  nonlocal calls
  calls+=1
  if calls==2:raise OSError("simulated receipt persistence failure")
  with original(config) as db:yield db
 d=image_bytes();h=headers(d);sid=str(uuid4());monkeypatch.setattr(m,"registry",failing);assert send(c,d,h,sid).status_code==503
 monkeypatch.setattr(m,"registry",original);assert send(c,d,h,sid).status_code==200;assert b.put_count==1
def test_corrupt_object_encryption(env):
 _,b,c=env;d=image_bytes();h=headers(d);sid=str(uuid4());assert send(c,d,h,sid).status_code==200
 b.objects[next(iter(b.objects))][1]["x-oss-server-side-encryption"]="none";assert send(c,d,h,sid).status_code==409
def test_capacity_limits(env):
 s,b,_=env;assert send(TestClient(create_app(replace(s,max_device_bytes=1),b))).status_code==429
 assert send(TestClient(create_app(replace(s,max_pending_bytes=1),b))).status_code==503;assert b.put_count==0
def test_explicit_state_and_bucket_required(monkeypatch):
 for n in ("TIMELENS_SPOOL_DIR","TIMELENS_DEVICE_TOKENS_FILE","TIMELENS_OSS_BUCKET"):monkeypatch.delenv(n,raising=False)
 with pytest.raises(KeyError):Settings.from_env()

@pytest.mark.parametrize("field,value",[("policy",True),("public_block",False),("versioning","Enabled")])
def test_bucket_security_change_during_put_never_acknowledged(env,field,value):
 s,b,c=env;b.after_put=lambda:setattr(b,field,value)
 r=send(c);assert r.status_code==503;assert "verified" not in r.json()

def test_pending_budget_is_reserved_before_streaming(env):
 import asyncio,httpx
 s,b,_=env;d=image_bytes();settings=replace(s,max_pending_bytes=len(d),max_inflight=2)
 app=create_app(settings,b)
 async def run():
  started=asyncio.Event();release=asyncio.Event()
  async def body():
   yield d[:1];started.set();await release.wait();yield d[1:]
  async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app),base_url="http://fixture") as c:
   first=asyncio.create_task(c.post("/v1/frames/"+str(uuid4()),headers=headers(d),content=body()))
   await started.wait()
   try:
    second=await c.post("/v1/frames/"+str(uuid4()),headers=headers(d),content=d)
    assert second.status_code==503
   finally:release.set()
   assert (await first).status_code==200
 asyncio.run(run());assert b.put_count==1

def test_cleanup_failure_never_leaks_capacity_or_slot(env,monkeypatch):
 from pathlib import Path
 s,b,_=env;c=TestClient(create_app(replace(s,max_inflight=1),b));d=image_bytes();h=headers(d);sid=str(uuid4());original=Path.unlink
 def fail(path,*args,**kwargs):
  if path.parent.name=="pending":raise OSError("simulated cleanup failure")
  return original(path,*args,**kwargs)
 monkeypatch.setattr(Path,"unlink",fail);assert send(c,d,h,sid).status_code==503
 monkeypatch.setattr(Path,"unlink",original);assert send(c,d,h,sid).status_code==200;assert b.put_count==1
