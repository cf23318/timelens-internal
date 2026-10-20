"""Synthetic-only lifecycle tests. No actual OSS client or credentials."""
import asyncio
import hashlib
import io
import json
import sqlite3
from dataclasses import replace
from uuid import uuid4
import httpx
import pytest
from fastapi import HTTPException
from fastapi.testclient import TestClient
from test_frames import env,headers,image_bytes,send,TASK,TOKEN,END
from timelens_upload import archive
from timelens_upload.app import create_app

WHO=('fixture-survey','fixture-device',TOKEN)

def fixture_upload(env):
    s,b,c=env;data=image_bytes();sid=str(uuid4());r=send(c,data,headers(data),sid)
    assert r.status_code==200,r.text
    return s,b,c,data,sid,r.json()['object_key']

def test_registration_without_frames_and_finish(env):
    s,b,c=env
    h=headers(image_bytes())
    r=c.post(f'/v1/archives/{TASK}/register',headers=h)
    assert r.status_code==200 and r.json()['frame_count']==0
    r=c.post(f'/v1/archives/{TASK}/finish',headers=h,json={'ended_at':'2026-10-20T00:00:00Z'})
    assert r.status_code==200,r.text
    assert r.json()['deadline_ms']==archive.timestamp('2026-10-20T00:00:00Z')+archive.RETENTION_MS
    assert not b.objects

def test_manifest_readback_and_managed_restore(env):
    s,b,c,data,sid,key=fixture_upload(env)
    page=archive.manifest(s,b,TASK,*WHO)
    assert len(page['frames'])==1 and page['next_after'] is None
    got,info=archive.read_frame(s,b,TASK,*WHO,sid)
    assert got==data and info['sha256']==hashlib.sha256(got).hexdigest()
    root=archive.restore(s,b,TASK,*WHO)
    assert (root/(sid+'.webp')).read_bytes()==data
    assert (root/'manifest.json').stat().st_mode&0o077==0
    assert 'Bearer' not in (root/'manifest.json').read_text()
    assert archive.restore(s,b,TASK,*WHO)==root

def test_partial_restore_crash_does_not_block_retry(env):
    s,b,c,data,sid,key=fixture_upload(env)
    root=s.spool_dir/'managed-restores'/TASK;root.mkdir(parents=True)
    (root/(sid+'.part')).write_bytes(b'partial')
    archive.restore(s,b,TASK,*WHO)
    assert (root/(sid+'.webp')).read_bytes()==data

@pytest.mark.parametrize('kind',['bytes','metadata','acl'])
def test_corrupt_cloud_readback_is_not_returned_or_restored(env,kind):
    s,b,c,data,sid,key=fixture_upload(env)
    if kind=='bytes':b.objects[key]=(b'corrupt',b.objects[key][1])
    elif kind=='metadata':b.objects[key][1]['x-oss-meta-sha256']='0'*64
    else:b.object_private=False
    with pytest.raises((ValueError,RuntimeError)):archive.read_frame(s,b,TASK,*WHO,sid)
    assert not (s.spool_dir/'managed-restores'/TASK/(sid+'.webp')).exists()

def test_cross_device_and_revoked_auth_cannot_read(env):
    s,b,c,data,sid,key=fixture_upload(env)
    for who in [('fixture-survey','other',TOKEN),('fixture-survey','fixture-device','wrong')]:
        with pytest.raises(HTTPException):archive.read_frame(s,b,TASK,*who,sid)
    s.tokens_file.write_text('{}')
    with pytest.raises(HTTPException):archive.manifest(s,b,TASK,*WHO)

def test_finish_shortens_never_extends_and_late_upload_keeps_deadline(env):
    s,b,c,data,sid,key=fixture_upload(env)
    end='2026-10-15T00:00:00Z'
    a=archive.finish(s,TASK,*WHO,end)
    assert a['finished']==1
    deadline=a['deadline_ms']
    assert archive.finish(s,TASK,*WHO,'2026-10-19T00:00:00Z')['deadline_ms']==deadline
    assert send(c).status_code==200
    assert archive.status(s,TASK,*WHO)['deadline_ms']==deadline
    h=headers(data,**{'X-TimeLens-Captured-At':'2026-10-16T00:00:00Z'})
    assert send(c,data,h).status_code==422

def test_deadline_boundaries_and_cleanup_all_managed_copies(env):
    s,b,c,data,sid,key=fixture_upload(env)
    a=archive.finish(s,TASK,*WHO,'2026-10-15T00:00:00Z')
    restore=archive.restore(s,b,TASK,*WHO)
    pending=s.spool_dir/'pending'/TASK;pending.mkdir();(pending/'crashed.webp').write_bytes(data)
    before=replace(s,clock=lambda:(a['deadline_ms']-1)/1000)
    assert archive.read_frame(before,b,TASK,*WHO,sid)[0]==data
    assert archive.cleanup(before,b)==[]
    expired=replace(s,clock=lambda:a['deadline_ms']/1000)
    with pytest.raises(HTTPException) as e:archive.read_frame(expired,b,TASK,*WHO,sid)
    assert e.value.status_code==410
    assert archive.cleanup(expired,b)==[TASK]
    assert not b.objects and not restore.exists() and not pending.exists()
    assert archive.cleanup(expired,b)==[TASK]  # retries also inspect a late landing object
    with sqlite3.connect(s.spool_dir/'receipts.sqlite3') as db:
        assert db.execute('SELECT COUNT(*) FROM frames').fetchone()[0]==0
        assert db.execute('SELECT deleted FROM archives').fetchone()[0]==1
        assert db.execute('PRAGMA quick_check').fetchone()[0]=='ok'

def test_delete_failure_is_visible_and_retry_compensates(env,monkeypatch):
    s,b,c,data,sid,key=fixture_upload(env)
    expired=replace(s,clock=lambda:(archive.timestamp(END)+archive.RETENTION_MS)/1000)
    original=b.delete_object
    monkeypatch.setattr(b,'delete_object',lambda key:(_ for _ in ()).throw(OSError('fixture')))
    with pytest.raises(RuntimeError):archive.cleanup(expired,b)
    assert (s.spool_dir/'retention-alert.json').exists() and key in b.objects
    monkeypatch.setattr(b,'delete_object',original)
    archive.cleanup(expired,b)
    assert not b.objects and not (s.spool_dir/'retention-alert.json').exists()

def test_active_io_prevents_false_success_then_retries(env):
    s,b,c,data,sid,key=fixture_upload(env)
    expired=replace(s,clock=lambda:(archive.timestamp(END)+archive.RETENTION_MS)/1000)
    with archive.lease(s,TASK):
        with pytest.raises(RuntimeError):archive.cleanup(expired,b)
        assert key in b.objects
    archive.cleanup(expired,b);assert not b.objects

def test_does_not_delete_other_task_or_legacy_objects(env):
    s,b,c,data,sid,key=fixture_upload(env)
    b.objects['timelens/v1/legacy.webp']=(data,{})
    b.objects['timelens/v2/surveys/other/unrelated.webp']=(data,{})
    expired=replace(s,clock=lambda:(archive.timestamp(END)+archive.RETENTION_MS)/1000)
    archive.cleanup(expired,b)
    assert len(b.objects)==2

def test_invalid_finish_auth_is_checked_before_body(env):
    s,b,c,data,sid,key=fixture_upload(env)
    async def run():
        read=False
        async def body():
            nonlocal read
            read=True;yield b'{';await asyncio.Event().wait()
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=create_app(s,b)),base_url='http://fixture') as client:
            r=await asyncio.wait_for(client.post(f'/v1/archives/{TASK}/finish',headers={**headers(data),'Authorization':'Bearer invalid'},content=body()),1)
            assert r.status_code==401 and read is False
    asyncio.run(run())

def test_policy_missing_changed_and_task_identity_fail_closed(env):
    s,b,c,data,sid,key=fixture_upload(env)
    assert send(c,data,headers(data,**{'X-TimeLens-Archive-End':'2026-11-01T00:00:00Z'})).status_code==409
    s.ends_file.unlink()
    assert send(c).status_code==503
    assert c.get(f'/v1/archives/{TASK}',headers=headers(data)).status_code==200

def test_expiry_during_get_returns_no_success(env):
    s,b,c,data,sid,key=fixture_upload(env)
    a=archive.status(s,TASK,*WHO);clock=[s.clock()]
    settings=replace(s,clock=lambda:clock[0])
    class Slow(io.BytesIO):
        def read(self,n=-1):
            value=super().read(n);clock[0]=a['deadline_ms']/1000;return value
    b.get_object=lambda key:Slow(data)
    with pytest.raises(HTTPException) as e:archive.read_frame(settings,b,TASK,*WHO,sid)
    assert e.value.status_code==410

def test_reconciliation_detects_missing_and_unindexed_objects(env):
    s,b,c,data,sid,key=fixture_upload(env)
    assert archive.reconcile(s,b,TASK,*WHO)=={'missing':0,'unexpected':0,'frame_count':1}
    b.objects[key+'.unexpected']=(data,b.objects[key][1])
    assert archive.reconcile(s,b,TASK,*WHO)['unexpected']==1
    del b.objects[key]
    assert archive.reconcile(s,b,TASK,*WHO)['missing']==1

def test_revoked_unacknowledged_put_is_compensated_before_expiry(env):
    s,b,c=env;b.after_put=lambda:s.tokens_file.write_text('{}')
    assert send(c).status_code==401 and b.objects
    archive.cleanup(s,b)
    assert not b.objects
    with sqlite3.connect(s.spool_dir/'receipts.sqlite3') as db:assert db.execute('SELECT COUNT(*) FROM frames').fetchone()[0]==0

def test_daemon_retries_after_failed_pass(env,monkeypatch):
    import sys
    from timelens_upload import cli
    s,b,c=env;calls=[]
    monkeypatch.setattr(cli.Settings,'from_env',classmethod(lambda cls:s))
    monkeypatch.setattr(cli,'make_bucket',lambda s:b)
    def fail(config,bucket):
        calls.append('cleanup')
        if len(calls)==1:raise RuntimeError('transient')
        raise KeyboardInterrupt()
    monkeypatch.setattr(cli.archive,'cleanup',fail)
    monkeypatch.setattr(cli.time,'sleep',lambda seconds:None)
    monkeypatch.setattr(sys,'argv',['timelens-archive','retention-daemon'])
    with pytest.raises(KeyboardInterrupt):cli.main()
    assert len(calls)==2

def test_orphan_snapshot_cannot_delete_a_newly_confirmed_frame(env,monkeypatch):
    from contextlib import contextmanager
    s,b,c,data,sid,key=fixture_upload(env)
    with sqlite3.connect(s.spool_dir/'receipts.sqlite3') as db:db.execute('UPDATE frames SET confirmed=0')
    original=archive.lease
    @contextmanager
    def accepted_before_lock(settings,task,exclusive=False):
        if exclusive:
            with sqlite3.connect(s.spool_dir/'receipts.sqlite3') as db:db.execute('UPDATE frames SET confirmed=1 WHERE snapshot_id=?',(sid,))
        with original(settings,task,exclusive):yield
    monkeypatch.setattr(archive,'lease',accepted_before_lock)
    archive.cleanup(s,b)
    assert key in b.objects

def test_manifest_pagination_has_no_missing_or_duplicate_ids(env):
    s,b,c=env;data=image_bytes()
    for i in range(501):assert send(c,data).status_code==200
    first=archive.manifest(s,b,TASK,*WHO)
    second=archive.manifest(s,b,TASK,*WHO,first['next_after'])
    ids=[f['snapshot_id'] for f in first['frames']+second['frames']]
    assert len(ids)==501 and len(set(ids))==501 and second['next_after'] is None

def test_frame_time_check_is_inside_lease_after_concurrent_finish(env,monkeypatch):
    from contextlib import contextmanager
    s,b,c=env;h=headers(image_bytes())
    assert c.post(f'/v1/archives/{TASK}/register',headers=h).status_code==200
    original=archive.lease;changed=[False]
    @contextmanager
    def finish_before_shared_lock(settings,task,exclusive=False):
        if not exclusive and not changed[0]:
            changed[0]=True
            archive.finish(settings,task,*WHO,'2026-09-30T00:00:00Z')
        with original(settings,task,exclusive):yield
    monkeypatch.setattr(archive,'lease',finish_before_shared_lock)
    assert send(c).status_code==422 and not b.objects

def test_finish_does_not_wait_for_all_frames_to_upload(env):
    s,b,c,data,sid,key=fixture_upload(env)
    # Successful frames and a failed local queue can share this task; finish is independent.
    r=c.post(f'/v1/archives/{TASK}/finish',headers=headers(data),json={'ended_at':'2026-10-15T00:00:00Z'})
    assert r.status_code==200
    early=r.json()['deadline_ms']
    expired=replace(s,clock=lambda:early/1000)
    archive.cleanup(expired,b)
    assert not b.objects

def test_equivalent_archive_end_timezone_retries_same_frame(env):
    s,b,c=env;d=image_bytes();h=headers(d);sid=str(uuid4())
    assert send(c,d,h,sid).status_code==200
    r=send(c,d,{**h,'X-TimeLens-Archive-End':'2026-11-01T04:00:00+11:00'},sid)
    assert r.status_code==200 and r.headers['Cache-Control']=='no-store'
