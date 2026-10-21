from datetime import datetime, timedelta, timezone
import copy
import json
from uuid import uuid4
import pytest
from fakes import FakeBucket
from oss_direct_fixture import grant
from timelens_upload import direct_admin as mod


def put(bucket, key, data, g, task, mime, extra=None):
    bucket.objects[key] = (data, {'Content-Type':mime,'x-oss-server-side-encryption':'AES256',
        **{'x-oss-meta-timelens-'+k:v for k,v in {'user':g['userId'],'device':g['deviceId'],'task':task,
           'sha256':mod.digest(data), **(extra or {})}.items()}})


def publish(bucket,g,task,frames,published=None,ended=None):
    prefix=g['prefix']+'manifests/'+task+'/'
    pages=[]
    for offset in range(0,len(frames),500):
        body=json.dumps({'format':'timelens-direct-page-v1','taskId':task,'frames':frames[offset:offset+500]},separators=(',',':')).encode()
        key=prefix+'pages/'+mod.digest(body)+'-'+str(uuid4())+'.json'
        put(bucket,key,body,g,task,'application/json')
        pages.append({'objectKey':key,'sha256':mod.digest(body),'sizeBytes':len(body),'frameCount':len(frames[offset:offset+500])})
    published=published or mod.now_ms()
    index={'format':'timelens-direct-index-v1','taskId':task,'userId':g['userId'],'deviceId':g['deviceId'],
           'archiveEndAt':g['archiveEndAt'],'endedAtMs':ended,'targetMs':172800000,'elapsedMs':0,
           'frameCount':len(frames),'sizeBytes':sum(f['sizeBytes'] for f in frames),'pages':pages,'publishedAtMs':published}
    key=prefix+f'indexes/{published:020}-'+str(uuid4())+'.json'
    put(bucket,key,json.dumps(index,separators=(',',':')).encode(),g,task,'application/json')
    return key,index


@pytest.fixture
def archive(tmp_path):
    g=grant();b=FakeBucket();b.bucket_name=g['bucket'];a=mod.Admin(tmp_path/'admin');a.enroll(g);task=str(uuid4())
    frames=[]
    for i in range(3):
        data=b'synthetic-webp-'+str(i).encode();sid=str(uuid4());capture=mod.now_ms()-1000
        day=datetime.fromtimestamp(capture/1000,timezone.utc).strftime('%Y/%m/%d')
        f={'snapshotId':sid,'capturedAtMs':capture,'displayId':1,'sha256':mod.digest(data),
           'sizeBytes':len(data),'objectKey':g['prefix']+'tasks/'+task+'/'+day+'/'+sid+'.webp'}
        frames.append(f);put(b,f['objectKey'],data,g,task,'image/webp',{'captured-at':str(capture),'display':'1'})
    key,index=publish(b,g,task,frames)
    return a,b,g,task,frames,key,index


def test_reconcile_full_readback_and_managed_restore(archive):
    a,b,g,t,frames,_,_=archive
    assert a.sync(b)['errors']==[]
    assert a.reconcile(b,t)=={'taskId':t,'finalInventory':False,'expected':3,'actual':3,'missing':0,'unexpected':0}
    assert a.verify(b,t,True)['verified']==3
    root=a.restores/t
    assert root.stat().st_mode & 0o077 == 0
    assert len(list(root.glob('*.webp')))==3
    for f in frames:
        file=root/(f['snapshotId']+'.webp')
        assert mod.digest(file.read_bytes())==f['sha256'] and file.stat().st_mode&0o077==0
    assert 'x-oss-signature' not in (a.root/'devices.json').read_text()


def test_missing_unexpected_and_ack_lost_object_count(archive):
    a,b,g,t,frames,_,_=archive;a.sync(b)
    # Expected frames are independent of client upload receipts; a lost ACK is still reconcilable.
    assert a.verify(b,t)['verified']==3
    b.objects.pop(frames[0]['objectKey']);r=a.reconcile(b,t);assert r['missing']==1
    with pytest.raises(ValueError):a.verify(b,t)
    b.objects[g['prefix']+'tasks/'+t+'/extra.webp']=(b'extra',{})
    assert a.reconcile(b,t)['unexpected']==1


@pytest.mark.parametrize('field,value',[('Content-Type','text/plain'),('x-oss-meta-timelens-device','wrong'),
    ('x-oss-meta-timelens-sha256','0'*64),('x-oss-server-side-encryption','')])
def test_original_metadata_and_checksums_fail_closed(archive,field,value):
    a,b,g,t,frames,_,_=archive;a.sync(b)
    key=frames[0]['objectKey'];data,headers=b.objects[key];b.objects[key]=(data,{**headers,field:value})
    with pytest.raises(ValueError):a.verify(b,t)


@pytest.mark.parametrize('kind',['missing_page','cross_task','duplicate','lost_frame','wrong_page_hash','bad_end'])
def test_bad_manifest_does_not_replace_last_good_inventory(archive,kind):
    a,b,g,t,frames,key,index=archive;a.sync(b);before=copy.deepcopy(a._task(t))
    bad=copy.deepcopy(frames)
    if kind=='cross_task':bad[0]['objectKey']=bad[0]['objectKey'].replace(t,str(uuid4()))
    if kind=='duplicate':bad.append(copy.deepcopy(bad[0]))
    if kind=='lost_frame':bad.pop()
    key,index=publish(b,g,t,bad,published=mod.now_ms()+1000,ended=1 if kind=='bad_end' else None)
    if kind=='missing_page':b.objects.pop(index['pages'][0]['objectKey'])
    if kind=='wrong_page_hash':
        index['pages'][0]['sha256']='0'*64
        put(b,key,json.dumps(index).encode(),g,t,'application/json')
    assert len(a.sync(b)['errors'])==1
    state=a._task(t)
    assert state['frames']==before['frames'] and state['deadlineMs']==before['deadlineMs']
    with pytest.raises(ValueError):a.reconcile(b,t)


def test_partial_page_without_index_is_not_empty_success_and_is_retained_for_cleanup(archive,monkeypatch):
    a,b,g,t,frames,key,index=archive;b.objects.pop(key)
    assert a.sync(b)['errors'] and not a._task(t)['inventoryComplete']
    with pytest.raises(ValueError):a.reconcile(b,t)
    monkeypatch.setattr(mod,'now_ms',lambda:mod.ms(g['archiveEndAt'])+mod.RETENTION_MS+1)
    assert a.retention(b,True)['deleted']==[t]
    assert not b.objects


def test_expiry_active_grant_drain_repeat_delete_and_no_renewal(archive,monkeypatch):
    a,b,g,t,frames,_,_=archive;a.sync(b);a.verify(b,t,True)
    current=mod.now_ms()
    # Actual early finish shortens retention, even if an already-issued grant remains valid.
    ended=current-mod.RETENTION_MS-1
    for f in frames:f['capturedAtMs']=ended-1000
    # Use an empty completed task to isolate lifecycle from frame timestamp validation.
    task=str(uuid4());key,index=publish(b,g,task,[],published=current+1,ended=ended)
    a.sync(b);assert a._task(task)['deadlineMs']<current
    assert a.retention(b,True)['blocked'][0]['taskId']==task
    with pytest.raises(ValueError):a.verify(b,task)
    with pytest.raises(ValueError):a.enroll(grant(study_end=g['archiveEndAt']))
    monkeypatch.setattr(mod,'now_ms',lambda:mod.ms(g['expiresAt'])+mod.GRANT_DRAIN_MS)
    assert task in a.retention(b,True)['deleted']
    # Late objects are swept again, never resurrecting the deleted task.
    put(b,g['prefix']+'tasks/'+task+'/2026/10/20/'+str(uuid4())+'.webp',b'late',g,task,'image/webp')
    assert task in a.retention(b,True)['deleted']
    assert not any('/'+task+'/' in k for k in b.objects)


def test_due_deletion_includes_images_manifests_and_managed_copies(archive,monkeypatch):
    a,b,g,t,_,_,_=archive;a.sync(b);a.verify(b,t,True)
    cutoff=a._task(t)['deadlineMs'];monkeypatch.setattr(mod,'now_ms',lambda:cutoff)
    assert a.retention(b)['dryRun'] and b.objects
    result=a.retention(b,True)
    assert result['deleted']==[t] and not b.objects and not (a.restores/t).exists()
    assert a._task(t)['frames']==[] and a._task(t)['deleted']
    with pytest.raises(ValueError):a.verify(b,t)
    with pytest.raises(ValueError):a.enroll(g)


def test_restore_symlink_refused_without_touching_target(archive,tmp_path):
    a,b,g,t,frames,_,_=archive;a.sync(b)
    outside=tmp_path/'outside';outside.mkdir();(a.restores/t).symlink_to(outside)
    with pytest.raises(ValueError):a.verify(b,t,True)
    assert not list(outside.iterdir())


def test_empty_task_is_complete_only_with_committed_index(archive):
    a,b,g,t,frames,_,_=archive;empty=str(uuid4());publish(b,g,empty,[])
    assert not a.sync(b)['errors'];assert a.verify(b,empty)['verified']==0


def test_paginated_manifest_over_500_frames(archive):
    a,b,g,t,frames,_,_=archive
    more=[]
    for i in range(1001):
        f=copy.deepcopy(frames[0]);f['snapshotId']=str(uuid4());f['objectKey']=f['objectKey'].replace(frames[0]['snapshotId'],f['snapshotId']);more.append(f)
    task=str(uuid4())
    for f in more:f['objectKey']=f['objectKey'].replace(t,task)
    publish(b,g,task,more)
    assert not a.sync(b)['errors'];assert len(a._task(task)['frames'])==1001


def test_private_registry_and_wrong_bucket(archive,tmp_path):
    a,b,g,t,_,_,_=archive;a.sync(b);b.bucket_name='wrong-bucket'
    with pytest.raises(ValueError):a.verify(b,t)
    public=tmp_path/'public';public.mkdir(mode=0o755)
    with pytest.raises(ValueError):mod.Admin(public)


def test_malformed_keys_and_one_failed_task_do_not_block_other_deletions(archive,monkeypatch):
    a,b,g,t,frames,_,_=archive;a.sync(b)
    other=str(uuid4());publish(b,g,other,[]);a.sync(b)
    # These names are permitted by the device POST prefix but do not match the application protocol.
    put(b,g['prefix']+'tasks/bad.webp',b'bad',g,t,'image/webp')
    put(b,g['prefix']+'manifests/not-a-uuid/bad.json',b'{}',g,t,'application/json')
    cutoff=a._task(t)['deadlineMs'];monkeypatch.setattr(mod,'now_ms',lambda:cutoff)
    original=b.delete_object
    def fail_one(key):
        if '/tasks/'+t+'/' in key:raise RuntimeError('simulated OSS deletion failure')
        original(key)
    monkeypatch.setattr(b,'delete_object',fail_one)
    result=a.retention(b,True)
    assert other in result['deleted'] and any(e.get('taskId')==t for e in result['blocked'])
    monkeypatch.setattr(b,'delete_object',original)
    result=a.retention(b,True)
    assert t in result['deleted'] and not b.objects


def test_final_inventory_is_distinct_from_intermediate(archive):
    a,b,g,t,frames,_,_=archive;a.sync(b)
    assert a.verify(b,t)['finalInventory'] is False
    publish(b,g,t,frames,published=mod.now_ms()+1000,ended=mod.now_ms())
    assert not a.sync(b)['errors'] and a.verify(b,t)['finalInventory'] is True
    publish(b,g,t,frames,published=mod.now_ms()+2000)
    assert a.sync(b)['errors'] and a._task(t)['endedAtMs'] is not None
