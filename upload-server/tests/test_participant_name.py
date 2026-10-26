from datetime import datetime, timezone
from uuid import uuid4
import copy
import pytest
from oss_direct_fixture import grant, verify
from timelens_upload import direct_admin as mod
from test_direct_admin import archive, publish
from test_shared_grant import form


@pytest.mark.parametrize('name', ['张三', 'John Smith', 'O’Neil', '张三 · A', 'A' * 64])
def test_names_are_literal_safe_path_segments(name):
    assert mod.valid_participant_name(name)
    prefix='timelens/direct/v1/users/test/devices/device/'
    assert mod.frame_key_matches(prefix+'tasks/task/'+name+'/2026/10/26/shot.webp', prefix, 'task', '2026/10/26', 'shot')


@pytest.mark.parametrize('name', ['', '.', '..', '../x', 'a/b', 'a\\b', ' 张三', '张三 ', 'a\nb', '\u202e张三', '\u200b张三', 'A' * 65])
def test_invalid_names_cannot_escape_or_disguise_a_folder(name):
    assert not mod.valid_participant_name(name)
    prefix='timelens/direct/v1/users/test/devices/device/'
    assert not mod.frame_key_matches(prefix+'tasks/task/'+name+'/2026/10/26/shot.webp', prefix, 'task', '2026/10/26', 'shot')


@pytest.mark.parametrize('shared', [False, True])
def test_existing_signed_grants_allow_name_subfolders(shared):
    g=grant(shared=shared);device=str(uuid4()) if shared else g['deviceId']
    prefix=g['prefix']+device+'/' if shared else g['prefix']
    fields=form(g, prefix+'tasks/'+str(uuid4())+'/测试 姓名/2026/10/26/'+str(uuid4())+'.webp')
    if shared: fields['x-oss-meta-timelens-device']=device
    assert verify(fields,b'synthetic')


def test_mixed_legacy_and_named_frames_reconcile_restore_and_expire(archive,monkeypatch):
    a,b,g,t,frames,_,index=archive
    for f,name in zip(frames[1:], ['张三','John Smith']):
        data,headers=b.objects.pop(f['objectKey'])
        day=datetime.fromtimestamp(f['capturedAtMs']/1000,timezone.utc).strftime('%Y/%m/%d')
        f['objectKey']=g['prefix']+'tasks/'+t+'/'+name+'/'+day+'/'+f['snapshotId']+'.webp'
        b.objects[f['objectKey']]=(data,headers)
    publish(b,g,t,frames,published=index['publishedAtMs']+1)
    assert a.sync(b)['errors']==[]
    assert a.reconcile(b,t)['missing']==0 and a.reconcile(b,t)['unexpected']==0
    assert a.verify(b,t,True)['verified']==3
    assert len(list((a.restores/t).glob('*.webp')))==3
    publish(b,g,t,copy.deepcopy(frames),published=index['publishedAtMs']+2)
    assert a.sync(b)['errors']==[] and a.verify(b,t)['verified']==3
    cutoff=a._task(t)['deadlineMs'];monkeypatch.setattr(mod,'now_ms',lambda:cutoff)
    assert a.retention(b,True)['deleted']==[t]
    assert not b.objects and not (a.restores/t).exists()
