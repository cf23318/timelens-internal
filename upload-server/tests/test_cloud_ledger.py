import copy
import pytest
from uuid import uuid4
from test_direct_admin import archive, publish
from timelens_upload import direct_admin as mod, cloud_ledger as cloud

def test_cloud_requests_identify_the_administrator_client(monkeypatch):
    # The deployed edge rejects Python-urllib's default agent before the Worker.
    # Keep authentication and the no-redirect handler on this real request path.
    import io
    class Response(io.BytesIO):
        status=200
    class Opener:
        def open(self,req,timeout):
            assert req.get_header('User-agent')=='TimeLens-Admin/1'
            assert req.get_header('Authorization')=='Bearer '+'a'*64
            assert req.full_url=='https://issuer.example.com/admin/sync'
            assert json.loads(req.data)=={'deadlines':[]}
            return Response(b'{"devices":[]}')
    def build(*handlers):
        assert any(isinstance(h,cloud.NoRedirect) for h in handlers)
        return Opener()
    import json
    monkeypatch.setattr(cloud.urllib.request,'build_opener',build)
    assert cloud.post({'issuerUrl':'https://issuer.example.com/','syncToken':'a'*64},'/admin/sync',{'deadlines':[]})=={'devices':[]}

def setup(a,g,monkeypatch):
    mod.write_private(a.root/'cloud-issuer.json',{'issuerUrl':'https://issuer.example.com/','syncToken':'b'*64})
    item={'deviceId':g['deviceId'],'userId':g['userId'],'bucket':g['bucket'],'scope':'device','plannedEndMs':mod.ms(g['archiveEndAt']),'deadlineMs':mod.ms(g['archiveEndAt'])+mod.RETENTION_MS,'lastGrantExpiresMs':mod.ms(g['expiresAt'])}
    calls=[]
    def post(config,path,payload):
        calls.append(copy.deepcopy(payload))
        for d in payload['deadlines']:
            if d['deviceId']==item['deviceId']:item['deadlineMs']=min(item['deadlineMs'],d['deadlineMs'])
        return {'format':'timelens-issuer-ledger-v1','atMs':mod.now_ms(),'devices':[copy.deepcopy(item)]}
    monkeypatch.setattr(cloud,'post',post)
    return item,calls

def test_cloud_renewal_expiry_is_imported_before_deletion(archive,monkeypatch):
    a,b,g,t,_,_,_=archive;a.sync(b);item,calls=setup(a,g,monkeypatch)
    now=mod.now_ms();task=str(uuid4());publish(b,g,task,[],published=now+1,ended=now-mod.RETENTION_MS-1)
    item['lastGrantExpiresMs']=now+8*86400000
    result=a.retention(b,True)
    assert task not in result['deleted'] and any(x.get('taskId')==task for x in result['blocked'])
    assert any(any(d['deviceId']==g['deviceId'] and d['deadlineMs']<now for d in c['deadlines']) for c in calls)
    assert mod.read_private(a.root/'devices.json')[g['deviceId']]['lastGrantExpiresMs']==item['lastGrantExpiresMs']

def test_cloud_unavailable_and_missing_config_fail_closed(archive,monkeypatch):
    a,b,g,t,_,_,_=archive;setup(a,g,monkeypatch);a.sync(b);before=copy.deepcopy(b.objects)
    def fail(*args):raise OSError('synthetic offline')
    monkeypatch.setattr(cloud,'post',fail)
    with pytest.raises(OSError):a.retention(b,True)
    assert b.objects==before
    (a.root/'cloud-issuer.json').unlink()
    with pytest.raises(ValueError,match='configuration missing'):a.retention(b,True)
    assert b.objects==before

def test_newly_discovered_early_deadline_crosses_cloud_barrier(archive,monkeypatch):
    a,b,g,t,_,_,_=archive;item,calls=setup(a,g,monkeypatch)
    now=mod.now_ms();task=str(uuid4());publish(b,g,task,[],published=now+1,ended=now-mod.RETENTION_MS-1)
    a.retention(b,False)
    assert item['deadlineMs']==a._task(task)['deadlineMs']
    assert len(calls)==4 # pre-discovery and post-discovery, each has atomic constraint/read

def test_ledger_identity_change_and_truncation_block_cleanup(archive,monkeypatch):
    a,b,g,t,_,_,_=archive;item,_=setup(a,g,monkeypatch);a.sync(b)
    item['bucket']='other-bucket'
    with pytest.raises(ValueError):a.retention(b,True)
    monkeypatch.setattr(cloud,'post',lambda *args:{'format':'timelens-issuer-ledger-v1','atMs':mod.now_ms(),'devices':[]})
    with pytest.raises(ValueError,match='lost enrolled'):a.retention(b,True)

def test_manual_renewal_does_not_remove_cloud_managed_marker(archive,monkeypatch):
    a,b,g,t,_,_,_=archive;setup(a,g,monkeypatch);a.sync(b);a.enroll(g)
    assert mod.read_private(a.root/'devices.json')[g['deviceId']]['cloudManaged']

def test_shared_unknown_device_uses_batch_deadline_and_can_be_cleaned(tmp_path,monkeypatch):
    from oss_direct_fixture import grant
    from test_shared_grant import bound
    from fakes import FakeBucket
    g=grant(shared=True);a=mod.Admin(tmp_path/'admin');a.enroll(g);b=FakeBucket();b.bucket_name=g['bucket']
    activated=str(uuid4());unknown=str(uuid4());item,calls=setup(a,bound(g,activated),monkeypatch);item['scope']='shared'
    task=str(uuid4());now=mod.now_ms();publish(b,bound(g,unknown),task,[],ended=now-mod.RETENTION_MS-1)
    result=a.retention(b,False)
    assert task in result['due']
    assert item['deadlineMs']==a._task(task)['deadlineMs']
    assert any(any(d['deviceId']==activated and d['deadlineMs']<now for d in c['deadlines']) for c in calls)
    devices=mod.read_private(a.root/'devices.json');assert not devices[f"shared:{g['userId']}:{unknown}"].get('cloudActivated')
    assert devices[f"shared:{g['userId']}:{activated}"]['cloudActivated']
    monkeypatch.setattr(mod,'now_ms',lambda:mod.ms(g['expiresAt'])+mod.GRANT_DRAIN_MS+1)
    assert task in a.retention(b,True)['deleted']
    assert not b.objects

def test_batch_cli_keeps_activation_code_for_lost_response_retry(tmp_path,monkeypatch,capsys):
    import sys
    from timelens_upload import cloud_issuer_cli as cli
    credentials=tmp_path/'admin.private.json';output=tmp_path/'pilot.activation.json'
    mod.write_private(credentials,{'issuerUrl':'https://issuer.example.com/','adminToken':'a'*64})
    monkeypatch.setattr(sys,'argv',['issuer','--credentials-file',str(credentials),'--batch-id','pilot','--study-end','2026-11-02T00:00:00Z','--activation-end','2026-11-01T00:00:00Z','--output',str(output)])
    seen=[]
    def post(config,path,payload,token_key):
        seen.append(copy.deepcopy(payload))
        if len(seen)==1:raise OSError('response lost')
        return {'format':'timelens-cloudflare-activation-v1','issuerUrl':config['issuerUrl'],'activationCode':payload['activationCode'],'batchId':'pilot','scope':'device','bucket':'test-bucket','archiveEndAt':payload['archiveEndAt']}
    monkeypatch.setattr(cli,'post',post)
    with pytest.raises(SystemExit):cli.main()
    pending=output.with_name(output.name+'.pending.private.json');assert pending.exists()
    cli.main();assert seen[0]==seen[1];assert not pending.exists();assert output.stat().st_mode&0o077==0
    captured=capsys.readouterr();assert seen[0]['activationCode'] not in captured.out+captured.err

def test_shared_alias_of_another_batch_device_does_not_shorten_wrong_batch(archive,monkeypatch):
    from oss_direct_fixture import grant
    from test_shared_grant import bound
    a,b,g,_,_,_,_=archive
    one,calls=setup(a,g,monkeypatch);gb=grant(user='shared-b',shared=True,study_end=g['archiveEndAt']);representative=str(uuid4())
    two={**one,'deviceId':representative,'userId':'shared-b','scope':'shared'}
    def post(config,path,payload):
        calls.append(copy.deepcopy(payload))
        for d in payload['deadlines']:
            row=one if d['deviceId']==one['deviceId'] else two
            row['deadlineMs']=min(row['deadlineMs'],d['deadlineMs'])
        return {'format':'timelens-issuer-ledger-v1','atMs':mod.now_ms(),'devices':[copy.deepcopy(one),copy.deepcopy(two)]}
    monkeypatch.setattr(cloud,'post',post)
    now=mod.now_ms();task=str(uuid4());publish(b,bound(gb,g['deviceId']),task,[],ended=now-mod.RETENTION_MS-1)
    a.retention(b,False)
    assert two['deadlineMs']<now
    assert one['deadlineMs']==mod.ms(g['archiveEndAt'])+mod.RETENTION_MS
    assert any(any(d['deviceId']==representative and d['deadlineMs']<now for d in c['deadlines']) for c in calls)

@pytest.mark.parametrize('u',['http://issuer.example.com/','https://u:p@issuer.example.com/','https://issuer.example.com/?token=bad','https://issuer.example.com/path'])
def test_https_root_only(u):
    with pytest.raises(ValueError):cloud.endpoint(u)
