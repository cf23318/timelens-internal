from datetime import datetime, timedelta, timezone
import copy
import json
import re
from uuid import uuid4

import pytest
from fastapi.testclient import TestClient

from oss_direct_fixture import grant, verify, ACCESS_ID, SECRET
from test_oss_grant import form
from test_direct_admin import publish, put
from fakes import FakeBucket
from timelens_upload import direct_admin as admin_mod
from timelens_upload.grant_web import create_app


def bound(g, device):
    return {**g, 'deviceId':device, 'prefix':g['prefix']+device+'/'}


def test_same_shared_config_accepts_two_device_folders_and_restricts_batch():
    g = grant(shared=True)
    assert g['deviceId'] == '*' and g['format'] == 'timelens-oss-shared-post-v1'
    for device in (str(uuid4()), str(uuid4())):
        fields = form(g, g['prefix']+device+'/tasks/example.webp')
        fields['x-oss-meta-timelens-device'] = device
        assert verify(fields,b'synthetic')
        fields['key'] = 'unrelated/'+device+'/example.webp'
        with pytest.raises(ValueError): verify(fields,b'synthetic')
    for name in ('x-oss-object-acl','x-oss-forbid-overwrite','x-oss-meta-timelens-study-end'):
        fields = form(g); fields[name] = 'tampered'
        with pytest.raises(ValueError): verify(fields,b'synthetic')
    end = admin_mod.ms(g['expiresAt'])/1000
    with pytest.raises(ValueError): verify(form(g),b'synthetic',datetime.fromtimestamp(end,timezone.utc))
    with pytest.raises(ValueError): grant(shared=True,days=8)
    assert SECRET not in json.dumps(g)


def test_shared_discovery_readback_renewal_and_batch_orphan_cleanup(tmp_path,monkeypatch):
    g = grant(shared=True); a = admin_mod.Admin(tmp_path/'admin'); a.enroll(g)
    b = FakeBucket(); b.bucket_name = g['bucket']; ids=[str(uuid4()),str(uuid4())]; tasks=[]
    for device in ids:
        task=str(uuid4());tasks.append(task); local=bound(g,device); capture=admin_mod.now_ms()-1000; sid=str(uuid4()); data=b'synthetic-shared-image'
        day=datetime.fromtimestamp(capture/1000,timezone.utc).strftime('%Y/%m/%d')
        frame={'snapshotId':sid,'capturedAtMs':capture,'displayId':1,'sha256':admin_mod.digest(data),'sizeBytes':len(data),'objectKey':local['prefix']+f'tasks/{task}/{day}/{sid}.webp'}
        put(b,frame['objectKey'],data,local,task,'image/webp',{'captured-at':str(capture),'display':'1'})
        publish(b,local,task,[frame],ended=admin_mod.now_ms())
    result = a.sync(b); assert not result['errors'] and set(result['updated']) == set(tasks)
    assert all(a.verify(b,task,True)['verified']==1 for task in tasks)
    # Same physical ID in a different batch must not overwrite the first scoped entry.
    other=grant(user='another-batch',shared=True);a.enroll(other)
    task=str(uuid4());publish(b,bound(other,ids[0]),task,[])
    assert not a.sync(b)['errors']
    devices=admin_mod.read_private(a.root/'devices.json'); assert len(devices)==3
    # A renewal updates group expiry; discovery later must inherit that expiry.
    renewed=grant(shared=True,now=datetime.now(timezone.utc)+timedelta(days=1),study_end=g['archiveEndAt'])
    a.enroll(renewed)
    late=str(uuid4());task_late=str(uuid4());publish(b,bound(g,late),task_late,[])
    a.sync(b)
    assert admin_mod.read_private(a.root/'devices.json')[f"shared:{g['userId']}:{late}"]['lastGrantExpiresMs']==admin_mod.ms(renewed['expiresAt'])
    b.objects[g['prefix']+'not-a-uuid/orphan']=(b'synthetic',{})
    assert a.sync(b)['errors']
    # All shared namespaces, including malformed keys, are repeatedly swept after expiry.
    monkeypatch.setattr(admin_mod,'now_ms',lambda:admin_mod.ms(g['archiveEndAt'])+admin_mod.RETENTION_MS+admin_mod.GRANT_DRAIN_MS+1)
    result=a.retention(b,True)
    assert g['userId'] in result['deletedGroups']
    assert not any(k.startswith(g['prefix']) for k in b.objects)
    b.objects[g['prefix']+'late-orphan']=(b'synthetic',{})
    assert g['userId'] in a.retention(b,True)['deletedGroups']
    assert not any(k.startswith(g['prefix']) for k in b.objects)


def test_shared_and_device_user_names_do_not_collide(tmp_path):
    a=admin_mod.Admin(tmp_path/'admin'); a.enroll(grant())
    with pytest.raises(ValueError):a.enroll(grant(shared=True))
    a.enroll(grant(user='shared-new',shared=True))
    with pytest.raises(ValueError):a.enroll(grant(user='shared-new'))


def test_shared_renewal_drain_is_authoritative_when_discovery_fails(tmp_path,monkeypatch):
    now=datetime.now(timezone.utc).replace(microsecond=0)
    planned=(now+timedelta(seconds=200)-timedelta(days=30)).isoformat()
    g=grant(shared=True,now=now,ttl_seconds=5,study_end=planned)
    a=admin_mod.Admin(tmp_path/'admin');a.enroll(g)
    b=FakeBucket();b.bucket_name=g['bucket'];device=str(uuid4())
    orphan=g['prefix']+device+'/orphan';b.objects[orphan]=(b'synthetic',{})
    a.sync(b)
    renewed=grant(shared=True,now=now,ttl_seconds=200,study_end=planned);a.enroll(renewed)
    def unavailable(*_args):raise OSError('synthetic discovery failure')
    monkeypatch.setattr(a,'_discover_shared',unavailable)
    monkeypatch.setattr(admin_mod,'now_ms',lambda:admin_mod.ms(renewed['expiresAt'])+1000)
    result=a.retention(b,True)
    assert orphan in b.objects and not result['deletedDevices'] and not result['deletedGroups']
    monkeypatch.setattr(admin_mod,'now_ms',lambda:admin_mod.ms(renewed['expiresAt'])+admin_mod.GRANT_DRAIN_MS+1)
    result=a.retention(b,True)
    assert orphan not in b.objects and device in result['deletedDevices']


def test_shared_partial_image_without_index_is_discovered_and_cannot_renew_past_earlier_deadline(tmp_path,monkeypatch):
    now=datetime.now(timezone.utc).replace(microsecond=0)
    g=grant(shared=True,now=now,study_end=(now-timedelta(days=29)+timedelta(hours=1)).isoformat())
    a=admin_mod.Admin(tmp_path/'admin');a.enroll(g);b=FakeBucket();b.bucket_name=g['bucket']
    device=str(uuid4());task=str(uuid4());key=g['prefix']+device+'/tasks/'+task+'/2026/01/01/'+str(uuid4())+'.webp'
    b.objects[key]=(b'synthetic',{})
    result=a.sync(b);assert result['errors'] and a._task(task)['sharedGroup']==g['userId']
    state=a._task(task);state['deadlineMs']=admin_mod.now_ms()+1000;a._save(state)
    with pytest.raises(ValueError):a.enroll(g)


def test_shared_task_uuid_conflict_does_not_block_other_devices(tmp_path):
    g=grant(shared=True);a=admin_mod.Admin(tmp_path/'admin');a.enroll(g)
    b=FakeBucket();b.bucket_name=g['bucket'];task=str(uuid4());devices=[str(uuid4()) for _ in range(3)]
    for device in devices[:2]:
        b.objects[g['prefix']+device+'/tasks/'+task+'/bad']=(b'synthetic',{})
    other=str(uuid4());publish(b,bound(g,devices[2]),other,[])
    result=a.sync(b)
    assert any(e.get('error')=='TaskIdentityConflict' for e in result['errors'])
    assert other in result['updated'] and a.verify(b,other)['verified']==0
    assert a._task(task)['deviceId'] in devices[:2]


def test_web_signing_is_loopback_origin_csrf_private_and_no_secret_response(tmp_path):
    credentials=tmp_path/'credentials.json';credentials.write_text(json.dumps({'accessKeyId':ACCESS_ID,'accessKeySecret':SECRET}));credentials.chmod(0o600)
    app=create_app(tmp_path/'admin',credentials,'fixture-private-bucket')
    client=TestClient(app,base_url='http://127.0.0.1:8877')
    page=client.get('/');assert page.status_code==200 and SECRET not in page.text
    token=re.search(r'"csrf": "([^"]+)"',page.text)[1]
    body={'userId':'shared-web-test','archiveEndAt':(datetime.now(timezone.utc)+timedelta(days=7)).isoformat()}
    assert client.post('/api/grants',json=body).status_code==403
    headers={'Origin':'http://127.0.0.1:8877','X-TimeLens-CSRF':token}
    assert client.post('/api/grants',json=body,headers={**headers,'Origin':'https://evil.invalid'}).status_code==403
    assert client.get('/',headers={'Host':'evil.invalid'}).status_code==403
    response=client.post('/api/grants',json=body,headers=headers);assert response.status_code==200
    result=response.json(); assert SECRET not in response.text and 'policy' not in response.text
    listing=client.get('/api/grants');assert len(listing.json())==1 and SECRET not in listing.text
    download=client.get(f"/api/grants/{result['id']}/download");assert download.status_code==200
    shared=download.json();assert shared['deviceId']=='*' and SECRET not in download.text
    assert (tmp_path/'admin/grants'/result['filename']).stat().st_mode & 0o077 == 0
    assert download.headers['cache-control']=='no-store'
    assert client.get('/api/grants/not-a-uuid/download').status_code==404
    assert client.post('/api/grants',json={**body,'credentialsFile':'/tmp/evil'},headers=headers).status_code==400
    assert client.post('/api/grants',json={**body,'archiveEndAt':'2020-01-01T00:00:00+00:00'},headers=headers).status_code==400
