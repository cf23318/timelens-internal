"""Task archives, readback, reconciliation and bounded retention.
No capture schedule, display denominator or duration accounting.
"""
from contextlib import contextmanager, nullcontext
from datetime import datetime
import hashlib
import json
import os
import shutil
import time
from uuid import uuid4
from fastapi import HTTPException
from oss2.utils import Crc64
from .config import private_dir
from .oss_backend import assert_private_bucket, assert_unversioned_bucket, delete_and_verify_absent, verified_head

RETENTION_MS = 30 * 86400_000

def now_ms(settings):
    return int(settings.clock() * 1000)

def timestamp(value):
    try:
        t = datetime.fromisoformat(value.replace('Z', '+00:00'))
        if t.tzinfo is None: raise ValueError()
        return int(t.timestamp() * 1000)
    except (AttributeError, TypeError, ValueError, OverflowError):
        raise HTTPException(400, 'Expected timezone-aware timestamp') from None

def schema(conn):
    conn.execute('''CREATE TABLE IF NOT EXISTS archives (
        task_id TEXT PRIMARY KEY, survey TEXT NOT NULL, device TEXT NOT NULL,
        planned_end_ms INTEGER NOT NULL, effective_end_ms INTEGER NOT NULL,
        deadline_ms INTEGER NOT NULL, finished INTEGER NOT NULL DEFAULT 0,
        deleted INTEGER NOT NULL DEFAULT 0)''')
    columns = {r[1] for r in conn.execute('PRAGMA table_info(frames)')}
    for name, kind in [('task_id', 'TEXT'), ('crc64', 'TEXT')]:
        if name not in columns: conn.execute(f'ALTER TABLE frames ADD COLUMN {name} {kind}')

@contextmanager
def registry(settings):
    from .app import registry as ingest_registry
    with ingest_registry(settings) as conn: yield conn

def get_archive(conn, task):
    row = conn.execute('SELECT task_id,survey,device,planned_end_ms,effective_end_ms,deadline_ms,finished,deleted FROM archives WHERE task_id=?', (task,)).fetchone()
    if row is None: raise HTTPException(404, 'Archive not registered')
    return dict(zip(('task_id','survey','device','planned_end_ms','effective_end_ms','deadline_ms','finished','deleted'), row))

def approved_end(settings, survey, device):
    try:
        fd = os.open(settings.ends_file, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, encoding='utf-8') as f:
            st = os.fstat(f.fileno())
            if st.st_size > 1024*1024 or st.st_uid != os.geteuid() or st.st_mode & 0o077: raise ValueError()
            value = json.load(f)[survey][device]
        return timestamp(value)
    except (OSError, TypeError, ValueError, KeyError, HTTPException):
        raise HTTPException(503, 'Approved archive end missing or invalid') from None

def enroll(settings, info):
    plan = timestamp(info['archive_end_at'])
    if plan != approved_end(settings, info['survey'], info['device']):
        raise HTTPException(409, 'Archive end differs from approved server policy')
    with registry(settings) as conn:
        conn.execute('INSERT OR IGNORE INTO archives(task_id,survey,device,planned_end_ms,effective_end_ms,deadline_ms) VALUES(?,?,?,?,?,?)',
                     (info['task_id'],info['survey'],info['device'],plan,plan,plan+RETENTION_MS))
        a = get_archive(conn, info['task_id'])
        if (a['survey'],a['device'],a['planned_end_ms']) != (info['survey'],info['device'],plan):
            raise HTTPException(409, 'Archive identity is immutable')
        require_live(settings, a)
        if timestamp(info['captured_at']) > a['effective_end_ms']: raise HTTPException(422, 'Frame captured after archive end')
        return a

def require_live(settings, a):
    if a['deleted'] or now_ms(settings) >= a['deadline_ms']: raise HTTPException(410, 'Archive retention expired')

def authorized_archive(settings, task, survey, device, token):
    if not settings.authorized(survey,device,token): raise HTTPException(401, 'Invalid device credential')
    with registry(settings) as conn: a = get_archive(conn, task)
    if (a['survey'],a['device']) != (survey,device): raise HTTPException(404, 'Archive not registered')
    require_live(settings,a)
    return a

@contextmanager
def lease(settings, task, exclusive=False):
    import fcntl
    from uuid import UUID
    if str(UUID(task)) != task: raise ValueError('Invalid task ID')
    root = private_dir(settings.spool_dir/'archive-locks')
    fd = os.open(root/(task+'.lock'),os.O_CREAT|os.O_RDWR|os.O_NOFOLLOW,0o600)
    try:
        try: fcntl.flock(fd,(fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH)|fcntl.LOCK_NB)
        except BlockingIOError: raise RuntimeError('Archive has in-flight I/O; cleanup must retry') from None
        yield
    finally: os.close(fd)

def status(settings, task, survey, device, token):
    a = authorized_archive(settings,task,survey,device,token)
    with registry(settings) as conn:
        count,size = conn.execute('SELECT COUNT(*),COALESCE(SUM(size),0) FROM frames WHERE task_id=? AND confirmed=1',(task,)).fetchone()
    return {**a,'frame_count':count,'size_bytes':size,'retention_days':30}

def finish(settings, task, survey, device, token, ended_at):
    end = timestamp(ended_at)
    with lease(settings,task,exclusive=True):
        a = authorized_archive(settings,task,survey,device,token)
        if end > now_ms(settings) or end > a['planned_end_ms']: raise HTTPException(400,'Invalid collection end')
        with registry(settings) as conn:
            a = get_archive(conn,task)
            latest = conn.execute("SELECT MAX(json_extract(identity,'$.captured_at')) FROM frames WHERE task_id=?",(task,)).fetchone()[0]
            if latest and timestamp(latest)>end: raise HTTPException(409,'End precedes an archived frame')
            effective = min(a['effective_end_ms'],end)
            conn.execute('UPDATE archives SET effective_end_ms=?,deadline_ms=?,finished=1 WHERE task_id=?',(effective,effective+RETENTION_MS,task))
    return status(settings,task,survey,device,token)

def manifest(settings,bucket,task,survey,device,token,after='',_leased=False):
    with (nullcontext() if _leased else lease(settings,task)):
        authorized_archive(settings,task,survey,device,token)
        with registry(settings) as conn:
            rows=conn.execute('SELECT snapshot_id,identity,object_key,crc64 FROM frames WHERE task_id=? AND confirmed=1 AND snapshot_id>? ORDER BY snapshot_id LIMIT 501',(task,after)).fetchall()
        frames=[]; assert_private_bucket(bucket)
        for sid,identity,key,crc in rows[:500]:
            info=json.loads(identity)
            verified_head(bucket,key,info['sha256'],info['size_bytes'],int(crc),{k:v for k,v in info.items() if k not in {'sha256','size_bytes'}})
            authorized_archive(settings,task,survey,device,token)
            frames.append({**info,'object_key':key})
        a=authorized_archive(settings,task,survey,device,token)
        return {'archive':a,'frames':frames,'next_after':rows[499][0] if len(rows)>500 else None}

def reconcile(settings,bucket,task,survey,device,token):
    with lease(settings,task):
        a=authorized_archive(settings,task,survey,device,token)
        assert_private_bucket(bucket)
        with registry(settings) as conn:
            expected={r[0] for r in conn.execute('SELECT object_key FROM frames WHERE task_id=? AND confirmed=1',(task,))}
        actual=set()
        for key in object_keys(bucket,prefix(a)):
            actual.add(key)
            if len(actual)>1_000_000:raise ValueError('Archive object count exceeds limit')
        authorized_archive(settings,task,survey,device,token)
        return {'missing':len(expected-actual),'unexpected':len(actual-expected),'frame_count':len(expected)}

def read_frame(settings,bucket,task,survey,device,token,snapshot,_leased=False):
    with (nullcontext() if _leased else lease(settings,task)):
        authorized_archive(settings,task,survey,device,token)
        with registry(settings) as conn:
            row=conn.execute('SELECT identity,object_key,crc64 FROM frames WHERE task_id=? AND snapshot_id=? AND confirmed=1',(task,snapshot)).fetchone()
        if row is None: raise HTTPException(404,'Frame not verified')
        info,key,crc=json.loads(row[0]),row[1],int(row[2]); assert_private_bucket(bucket)
        verified_head(bucket,key,info['sha256'],info['size_bytes'],crc,{k:v for k,v in info.items() if k not in {'sha256','size_bytes'}})
        stream=bucket.get_object(key); data=bytearray();digest=hashlib.sha256();actual_crc=Crc64();started=time.monotonic()
        try:
            while True:
                chunk=stream.read(64*1024)
                authorized_archive(settings,task,survey,device,token)
                if time.monotonic()-started>120: raise RuntimeError('Readback deadline exceeded')
                if not chunk:break
                data.extend(chunk);digest.update(chunk);actual_crc.update(chunk)
                if len(data)>info['size_bytes']:raise ValueError('Readback too large')
        finally: stream.close()
        if len(data)!=info['size_bytes'] or digest.hexdigest()!=info['sha256'] or actual_crc.crc!=crc:
            raise ValueError('Readback hash/size/CRC mismatch')
        authorized_archive(settings,task,survey,device,token); assert_private_bucket(bucket)
        return bytes(data),info

def prefix(a):
    return f"timelens/v2/surveys/{a['survey']}/devices/{a['device']}/tasks/{a['task_id']}/"

def object_keys(bucket, root):
    marker=''
    while True:
        listing=bucket.list_objects(prefix=root,marker=marker,max_keys=1000)
        for obj in listing.object_list:
            if not obj.key.startswith(root):raise ValueError('OSS listing escaped task prefix')
            yield obj.key
        if not listing.is_truncated:break
        if not listing.next_marker or listing.next_marker==marker:raise ValueError('Invalid OSS pagination')
        marker=listing.next_marker

def safe_remove(path):
    if path.is_symlink(): raise RuntimeError('Managed directory is a symlink')
    if path.exists():shutil.rmtree(path)

def cleanup(settings,bucket):
    errors=[];deleted=[]
    with registry(settings) as conn:
        tasks=[r[0] for r in conn.execute('SELECT task_id FROM archives WHERE deadline_ms<=?',(now_ms(settings),))]
    with registry(settings) as conn:
        orphans=conn.execute('SELECT snapshot_id,task_id,object_key FROM frames WHERE task_id IS NOT NULL AND confirmed=0').fetchall()
    for sid,task,key in orphans:
        try:
            with lease(settings,task,exclusive=True):
                with registry(settings) as conn:
                    current=conn.execute('SELECT object_key FROM frames WHERE snapshot_id=? AND task_id=? AND confirmed=0',(sid,task)).fetchone()
                    a=get_archive(conn,task)
                if current is None:continue
                if current[0]!=key or not key.startswith(prefix(a)):raise ValueError('Orphan prefix mismatch')
                assert_unversioned_bucket(bucket);delete_and_verify_absent(bucket,key)
                with registry(settings) as conn:conn.execute('DELETE FROM frames WHERE snapshot_id=? AND confirmed=0',(sid,))
        except Exception as error:errors.append({'task_id':task,'error':type(error).__name__})
    for task in tasks:
        try:
            with lease(settings,task,exclusive=True):
                with registry(settings) as conn:a=get_archive(conn,task)
                if now_ms(settings)<a['deadline_ms']:continue
                assert_unversioned_bucket(bucket)
                for key in list(object_keys(bucket,prefix(a))):delete_and_verify_absent(bucket,key)
                if list(object_keys(bucket,prefix(a))):raise RuntimeError('OSS prefix survived cleanup')
                for name in ('pending','managed-restores'):safe_remove(settings.spool_dir/name/task)
                with registry(settings) as conn:
                    conn.execute('PRAGMA secure_delete=ON')
                    conn.execute('DELETE FROM frames WHERE task_id=?',(task,))
                    conn.execute('UPDATE archives SET deleted=1 WHERE task_id=?',(task,))
                deleted.append(task)
        except Exception as error:errors.append({'task_id':task,'error':type(error).__name__})
    with registry(settings) as conn:
        conn.commit();conn.execute('VACUUM');conn.execute('PRAGMA wal_checkpoint(TRUNCATE)')
    alert=settings.spool_dir/'retention-alert.json'
    if errors:
        alert.write_text(json.dumps({'failed':errors}));alert.chmod(0o600)
        raise RuntimeError('Retention cleanup incomplete; see private alert and retry')
    alert.unlink(missing_ok=True)
    return deleted

def restore(settings,bucket,task,survey,device,token):
    with lease(settings,task,exclusive=True):
        a=authorized_archive(settings,task,survey,device,token)
        base=private_dir(settings.spool_dir/'managed-restores');root=private_dir(base/task)
        after='';entries=[]
        while True:
            page=manifest(settings,bucket,task,survey,device,token,after,_leased=True)
            for info in page['frames']:
                data,_=read_frame(settings,bucket,task,survey,device,token,info['snapshot_id'],_leased=True)
                authorized_archive(settings,task,survey,device,token)
                target=root/(info['snapshot_id']+'.webp')
                if target.exists():
                    if target.is_symlink() or hashlib.sha256(target.read_bytes()).hexdigest()!=info['sha256']:raise ValueError('Existing restore differs')
                else:
                    part=target.with_suffix('.'+str(uuid4())+'.part')
                    fd=os.open(part,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
                    try:
                        with os.fdopen(fd,'wb') as f:f.write(data);f.flush();os.fsync(f.fileno())
                        authorized_archive(settings,task,survey,device,token);os.link(part,target)
                    finally:part.unlink(missing_ok=True)
                entries.append(info)
            after=page['next_after']
            if after is None:break
        authorized_archive(settings,task,survey,device,token)
        fd=os.open(root/'manifest.json',os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_NOFOLLOW,0o600)
        with os.fdopen(fd,'w') as f:json.dump({'archive':a,'frames':entries},f)
        authorized_archive(settings,task,survey,device,token)
        return root
