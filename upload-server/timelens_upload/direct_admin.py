"""Administrator-only direct OSS inventory, readback and retention. No image proxy.

All grants must be issued/enrolled in this same private registry. Keep the daemon
running on an always-on administrator machine. An offline Mac cannot delete OSS.
"""
import argparse
import copy
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import time
from uuid import UUID, uuid4

from oss2.utils import Crc64
from .archive import object_keys
from .oss_backend import assert_private_bucket, assert_private_object, delete_and_verify_absent, header_value
from .oss_grant import utc, iso, private_json, MAX_BYTES

RETENTION_MS = 30*86400000
GRANT_DRAIN_MS = 120000  # client requests time out at 60s; expired tasks are rescanned on every pass
MAX_FRAMES = 200000


def now_ms():
    return int(time.time()*1000)


def ms(value):
    return int(utc(value).timestamp()*1000)


def uid(value):
    if not isinstance(value, str) or str(UUID(value)) != value:
        raise ValueError('Invalid canonical ID')
    return value


def private_dir(path):
    path = Path(path)
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError('Administrator directory must be owned by this user, mode 0700, not a symlink')
    return path


def strict_json(data):
    def pairs(items):
        result = {}
        for k, v in items:
            if k in result:
                raise ValueError('Duplicate JSON field')
            result[k] = v
        return result
    return json.loads(data, object_pairs_hook=pairs, parse_constant=lambda _: (_ for _ in ()).throw(ValueError('Non-finite number')))


def read_private(path, default=None):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    except FileNotFoundError:
        return default
    with os.fdopen(fd, 'rb') as f:
        info = os.fstat(f.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077 or info.st_size > 128*1024*1024:
            raise ValueError('Invalid private registry file')
        return strict_json(f.read())


def write_private(path, value):
    temp = path.with_name(path.name+'.'+str(uuid4())+'.part')
    fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(fd, 'w') as out:
            json.dump(value, out, separators=(',', ':'), allow_nan=False)
            out.flush(); os.fsync(out.fileno())
        if path.is_symlink():
            raise ValueError('Registry is a symlink')
        os.replace(temp, path)
    finally:
        temp.unlink(missing_ok=True)


def integer(value, minimum=0, maximum=4102444800000):
    if type(value) is not int or not minimum <= value <= maximum:
        raise ValueError('Invalid bounded integer')
    return value


def digest(data):
    return hashlib.sha256(data).hexdigest()


def valid_participant_name(name):
    return (isinstance(name, str) and name == name.strip() and 0 < len(name) <= 64
            and len(name.encode('utf-8')) <= 256 and name not in ('.', '..')
            and not any(c in '/\\' or ord(c) < 32 or 0x7f <= ord(c) <= 0x9f
                        or 0x200b <= ord(c) <= 0x200f or 0x202a <= ord(c) <= 0x202e
                        or 0x2060 <= ord(c) <= 0x206f or ord(c) == 0xfeff for c in name))


def frame_key_matches(key, prefix, task, day, snapshot):
    """Accept legacy keys and one literal name segment within the same task scope."""
    root = f'{prefix}tasks/{task}/'
    tail = f'{day}/{snapshot}.webp'
    if key == root + tail:
        return True
    if not isinstance(key, str) or not key.startswith(root) or not key.endswith('/' + tail):
        return False
    name = key[len(root):-len('/' + tail)]
    return valid_participant_name(name)


class Admin:
    def __init__(self, root):
        self.root = private_dir(root)
        self.tasks = private_dir(self.root/'tasks')
        self.restores = private_dir(self.root/'restores')

    @contextmanager
    def lock(self):
        # Mac/Linux administrator tool only. Client uploads cannot take this lock;
        # retention additionally waits for ALL registered device grants to expire.
        import fcntl
        fd = os.open(self.root/'admin.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            yield
        finally:
            os.close(fd)

    def enroll(self, grant):
        if grant.get('format') == 'timelens-oss-shared-post-v1':
            return self._enroll_shared(grant)
        device = uid(grant['deviceId']); user = grant['userId']; bucket = grant['bucket']
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,63}', user) or not re.fullmatch(r'[a-z0-9][a-z0-9-]{1,61}[a-z0-9]', bucket):
            raise ValueError('Invalid grant identity')
        prefix = f'timelens/direct/v1/users/{user}/devices/{device}/'
        if grant['prefix'] != prefix or grant['endpoint'] != f'https://{bucket}.oss-cn-beijing.aliyuncs.com/':
            raise ValueError('Invalid scoped destination')
        plan = ms(grant['archiveEndAt']); expiry = ms(grant['expiresAt']); issued = ms(grant['issuedAt'])
        if not 0 < expiry-issued <= 7*86400000 or expiry > plan+RETENTION_MS or expiry <= now_ms():
            raise ValueError('Grant exceeds maximum retention or lifetime')
        with self.lock():
            devices = read_private(self.root/'devices.json', {})
            if user in read_private(self.root/'shared-groups.json', {}):
                raise ValueError('Shared batch ID cannot be used for a device-scoped grant')
            entry = {'deviceId': device, 'userId': user, 'bucket': bucket, 'prefix': prefix, 'plannedEndMs': plan}
            old = devices.get(device)
            if old and any(old[k] != v for k, v in entry.items()):
                raise ValueError('Device destination and approved end are immutable')
            deadlines = [t['deadlineMs'] for t in self._tasks() if all(t[k] == entry[k] for k in ('deviceId','userId','bucket','prefix'))]
            if deadlines and now_ms()>=min(deadlines):
                raise ValueError('Device has an expired task; renew under a new approved profile instead')
            if deadlines and expiry+GRANT_DRAIN_MS > min(deadlines):
                raise ValueError('Renewal would outlive an existing task deletion deadline')
            entry['lastGrantExpiresMs'] = max(expiry, (old or {}).get('lastGrantExpiresMs', 0))
            if (old or {}).get('cloudManaged'): entry['cloudManaged']=True
            devices[device] = entry
            write_private(self.root/'devices.json', devices)
        return entry

    def _enroll_shared(self, grant):
        user = grant['userId']; bucket = grant['bucket']
        if grant['deviceId'] != '*' or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,63}', user) or not re.fullmatch(r'[a-z0-9][a-z0-9-]{1,61}[a-z0-9]', bucket):
            raise ValueError('Invalid shared grant identity')
        prefix = f'timelens/direct/v1/users/{user}/devices/'
        if grant['prefix'] != prefix or grant['endpoint'] != f'https://{bucket}.oss-cn-beijing.aliyuncs.com/':
            raise ValueError('Invalid shared destination')
        plan = ms(grant['archiveEndAt']); expiry = ms(grant['expiresAt']); issued = ms(grant['issuedAt'])
        if not 0 < expiry-issued <= 7*86400000 or expiry > plan+RETENTION_MS or expiry <= now_ms():
            raise ValueError('Shared grant exceeds maximum retention or lifetime')
        with self.lock():
            groups = read_private(self.root/'shared-groups.json', {})
            devices = read_private(self.root/'devices.json', {})
            if any(d['userId'] == user and not d.get('sharedGroup') for d in devices.values()):
                raise ValueError('Device-scoped user ID cannot be reused as a shared batch')
            entry = {'userId':user, 'bucket':bucket, 'prefix':prefix, 'plannedEndMs':plan}
            old = groups.get(user)
            if old and any(old[k] != v for k,v in entry.items()):
                raise ValueError('Shared batch destination and approved end are immutable')
            deadlines = [t['deadlineMs'] for t in self._tasks() if t.get('sharedGroup') == user]
            if deadlines and (now_ms() >= min(deadlines) or expiry+GRANT_DRAIN_MS > min(deadlines)):
                raise ValueError('Shared renewal would outlive a known task deletion deadline')
            entry['lastGrantExpiresMs'] = max(expiry, (old or {}).get('lastGrantExpiresMs',0))
            if (old or {}).get('cloudManaged'): entry['cloudManaged']=True
            groups[user] = entry
            # This is the authoritative batch expiry, including devices discovered later.
            write_private(self.root/'shared-groups.json', groups)
        return entry

    @staticmethod
    def _device_key(device):
        return f"shared:{device['sharedGroup']}:{device['deviceId']}" if device.get('sharedGroup') else device['deviceId']

    def _discover_shared(self, bucket, devices):
        errors = []
        for group in read_private(self.root/'shared-groups.json', {}).values():
            if group['bucket'] != bucket.bucket_name: continue
            count = 0
            for key in object_keys(bucket,group['prefix']):
                count += 1
                if count > 1000000: raise ValueError('Shared batch object limit exceeded')
                try:
                    device = uid(key.removeprefix(group['prefix']).split('/')[0])
                except (ValueError, TypeError):
                    errors.append({'sharedGroup':group['userId'],'error':'UnknownSharedNamespace'})
                    continue
                entry = {**group, 'deviceId':device, 'prefix':group['prefix']+device+'/', 'sharedGroup':group['userId']}
                registry_key = self._device_key(entry)
                old = devices.get(registry_key)
                if old and any(old[k] != entry[k] for k in ('deviceId','userId','bucket','prefix','plannedEndMs','sharedGroup')):
                    raise ValueError('Shared device registry conflict')
                if (old or {}).get('cloudActivated'): entry['cloudActivated']=True
                devices[registry_key] = entry
        write_private(self.root/'devices.json',devices)
        return errors

    def _tasks(self):
        return [read_private(p) for p in sorted(self.tasks.glob('*.json'))]

    def _task(self, task):
        value = read_private(self.tasks/(uid(task)+'.json'))
        if value is None:
            raise ValueError('Task is not enrolled')
        return value

    def _save(self, task):
        write_private(self.tasks/(uid(task['taskId'])+'.json'), task)

    def _require_live(self, task):
        if task['deleted'] or now_ms() >= task['deadlineMs']:
            raise ValueError('Task retention expired; original readback is disabled')

    def _new_task(self, device, task):
        return {**device, 'taskId': uid(task), 'effectiveEndMs': device['plannedEndMs'],
                'deadlineMs': device['plannedEndMs']+RETENTION_MS, 'deleted': False,
                'inventoryComplete': False, 'endedAtMs':None, 'publishedAtMs': 0, 'frames': []}

    def _json_object(self, bucket, key, device, task, limit=MAX_BYTES):
        head = bucket.head_object(key)
        if head.content_length <= 0 or head.content_length > limit:
            raise ValueError('Manifest object size is invalid')
        assert_private_object(bucket, key)
        expected = {'user': device['userId'], 'device': device['deviceId'], 'task': task}
        if device.get('sharedGroup'):
            expected['study-end'] = iso(datetime.fromtimestamp(device['plannedEndMs']/1000,timezone.utc))
        for name, value in expected.items():
            if header_value(head, 'x-oss-meta-timelens-'+name) != value:
                raise ValueError('Manifest identity differs')
        if header_value(head, 'Content-Type') != 'application/json' or header_value(head, 'x-oss-server-side-encryption') != 'AES256':
            raise ValueError('Manifest MIME or encryption differs')
        data = self._read(bucket, key, head.content_length)
        crc = Crc64(); crc(data)
        if digest(data) != header_value(head, 'x-oss-meta-timelens-sha256') or str(crc.crc) != str(header_value(head, 'x-oss-hash-crc64ecma')):
            raise ValueError('Manifest readback checksum differs')
        return strict_json(data), data

    @staticmethod
    def _read(bucket, key, size):
        stream = bucket.get_object(key); data = bytearray(); started = time.monotonic()
        try:
            while True:
                chunk = stream.read(65536)
                if time.monotonic()-started > 120:
                    raise ValueError('Readback exceeded total deadline')
                if not chunk: break
                data.extend(chunk)
                if len(data) > size: raise ValueError('Readback exceeds declared length')
        finally: stream.close()
        if len(data) != size: raise ValueError('Truncated readback')
        return bytes(data)

    def _load_index(self, bucket, device, task, key):
        index, _ = self._json_object(bucket, key, device, task, 1024*1024)
        if set(index) != {'format','taskId','userId','deviceId','archiveEndAt','endedAtMs','targetMs','elapsedMs','frameCount','sizeBytes','pages','publishedAtMs'}:
            raise ValueError('Invalid index schema')
        if index['format'] != 'timelens-direct-index-v1' or index['taskId'] != task or index['userId'] != device['userId'] or index['deviceId'] != device['deviceId'] or ms(index['archiveEndAt']) != device['plannedEndMs']:
            raise ValueError('Index identity differs')
        published = integer(index['publishedAtMs'], 1, now_ms()+300000)
        if not key.rsplit('/',1)[1].startswith(f'{published:020}-'):
            raise ValueError('Index revision differs from object key')
        integer(index['targetMs'], 1, 48*3600000); integer(index['elapsedMs'], 0, 48*3600000)
        count = integer(index['frameCount'], 0, MAX_FRAMES); integer(index['sizeBytes'],0,MAX_FRAMES*MAX_BYTES)
        end = device['plannedEndMs']
        if index['endedAtMs'] is not None:
            end = min(end, integer(index['endedAtMs'], 1, now_ms()))
        pages = index['pages']
        if not isinstance(pages,list) or len(pages) != (count+499)//500:
            raise ValueError('Missing or extra manifest pages')
        frames = {}; page_keys = set()
        page_root = device['prefix']+'manifests/'+task+'/pages/'
        for ref in pages:
            if set(ref) != {'objectKey','sha256','sizeBytes','frameCount'} or not re.fullmatch(r'[0-9a-f]{64}',ref['sha256']):
                raise ValueError('Invalid page reference')
            page_key = ref['objectKey']
            if not re.fullmatch(re.escape(page_root+ref['sha256']+'-')+r'[0-9a-f-]{36}\.json',page_key) or page_key in page_keys:
                raise ValueError('Page escaped task scope or was duplicated')
            page_keys.add(page_key)
            page, data = self._json_object(bucket, page_key, device, task)
            if len(data) != ref['sizeBytes'] or digest(data) != ref['sha256'] or set(page) != {'format','taskId','frames'} or page['format'] != 'timelens-direct-page-v1' or page['taskId'] != task:
                raise ValueError('Page hash, size or identity differs')
            if not isinstance(page['frames'],list) or not 1 <= len(page['frames']) <= 500 or len(page['frames']) != ref['frameCount']:
                raise ValueError('Page frame count differs')
            for frame in page['frames']:
                if set(frame) != {'snapshotId','capturedAtMs','displayId','sha256','sizeBytes','objectKey'}:
                    raise ValueError('Invalid frame schema')
                sid = uid(frame['snapshotId']); capture = integer(frame['capturedAtMs'], 1, end)
                integer(frame['displayId'],0,4294967295); integer(frame['sizeBytes'],1,MAX_BYTES)
                if not re.fullmatch(r'[0-9a-f]{64}', frame['sha256']) or sid in frames:
                    raise ValueError('Duplicate frame or invalid hash')
                day = datetime.fromtimestamp(capture/1000, timezone.utc).strftime('%Y/%m/%d')
                if not frame_key_matches(frame['objectKey'], device['prefix'], task, day, sid):
                    raise ValueError('Frame escaped task scope')
                frames[sid] = frame
        if len(frames) != count or sum(f['sizeBytes'] for f in frames.values()) != index['sizeBytes']:
            raise ValueError('Manifest total differs')
        return index, frames, end

    def sync(self, bucket):
        with self.lock():
            from .cloud_ledger import synchronize
            synchronize(self,bucket.bucket_name)
            return self._sync(bucket)

    def _sync(self, bucket):
        assert_private_bucket(bucket)
        devices = read_private(self.root/'devices.json', {}); updated = []
        errors = self._discover_shared(bucket,devices)
        for device in devices.values():
            if device['bucket'] != bucket.bucket_name: continue
            found = {}; image_tasks = set(); total = 0
            # Discover image-only/partial uploads too, so a lost index cannot evade retention.
            for key in object_keys(bucket, device['prefix']+'tasks/'):
                total += 1
                if total > 1000000: raise ValueError('Device object limit exceeded')
                match = re.fullmatch(re.escape(device['prefix']+'tasks/')+r'([0-9a-f-]{36})/(?:[^/]+/)?\d{4}/\d{2}/\d{2}/[0-9a-f-]{36}\.webp',key)
                if not match:
                    # Unknown suffixes still belong to a canonical task, if present.
                    candidate=key.removeprefix(device['prefix']+'tasks/').split('/')[0]
                    try: image_tasks.add(uid(candidate))
                    except ValueError: errors.append({'sharedGroup':device.get('sharedGroup'),'deviceId':device['deviceId'],'error':'UnknownImageNamespace'})
                    continue
                try: image_tasks.add(uid(match[1]))
                except ValueError: errors.append({'sharedGroup':device.get('sharedGroup'),'deviceId':device['deviceId'],'error':'UnknownTaskId'})
            for key in object_keys(bucket, device['prefix']+'manifests/'):
                total += 1
                if total > 1000000: raise ValueError('Device object limit exceeded')
                relative = key.removeprefix(device['prefix']+'manifests/')
                try: task = uid(relative.split('/')[0])
                except ValueError:
                    errors.append({'sharedGroup':device.get('sharedGroup'),'deviceId':device['deviceId'],'error':'UnknownManifestNamespace'});continue
                image_tasks.add(task)
                if re.fullmatch(r'[0-9a-f-]{36}/indexes/\d{20}-[0-9a-f-]{36}\.json',relative):
                    found[task] = max(found.get(task,''),key)
            for task in sorted(image_tasks):
                old = read_private(self.tasks/(task+'.json'))
                if old and any(old[k] != device[k] for k in ('deviceId','userId','bucket','prefix','plannedEndMs')):
                    errors.append({'conflictingTaskId':task,'deviceId':device['deviceId'],'sharedGroup':device.get('sharedGroup'),'error':'TaskIdentityConflict'})
                    continue
                state = copy.deepcopy(old) if old else self._new_task(device,task)
                # Tombstones never become live again. Cleanup repeatedly sweeps these scopes.
                if state['deleted']: continue
                try:
                    if task in found:
                        index, frames, end = self._load_index(bucket,device,task,found[task])
                        if index['publishedAtMs'] < state['publishedAtMs']:
                            raise ValueError('Manifest revision rolled back')
                        if state.get('endedAtMs') is not None and index['endedAtMs'] is None:
                            raise ValueError('Completed task cannot become active again')
                        previous = {f['snapshotId']:f for f in state['frames']}
                        if any(frames.get(sid) != f for sid,f in previous.items()):
                            raise ValueError('Manifest lost or changed a previous frame')
                        state.update(effectiveEndMs=min(state['effectiveEndMs'],end), inventoryComplete=True,
                                     publishedAtMs=index['publishedAtMs'], frames=list(frames.values()),endedAtMs=index['endedAtMs'])
                        if any(f['capturedAtMs']>state['effectiveEndMs'] for f in state['frames']):
                            raise ValueError('Frame exceeds known end')
                        state['deadlineMs'] = state['effectiveEndMs']+RETENTION_MS
                    else:
                        raise ValueError('Task has no committed inventory index')
                    state.pop('syncError',None); updated.append(task)
                except Exception as error:
                    # Preserve previously verified expectations and the earliest deletion deadline.
                    state = old or self._new_task(device,task)
                    state['syncError'] = type(error).__name__
                    errors.append({'taskId':task,'error':type(error).__name__})
                self._save(state)
        return {'updated':updated,'errors':errors}

    def _reconcile(self, bucket, task):
        self._require_live(task); assert_private_bucket(bucket)
        if not task['inventoryComplete'] or task.get('syncError'):
            raise ValueError('No complete verified inventory; cannot declare reconciliation successful')
        expected = {f['objectKey'] for f in task['frames']}
        actual = set(object_keys(bucket, task['prefix']+'tasks/'+task['taskId']+'/'))
        self._require_live(task)
        return {'taskId':task['taskId'],'finalInventory':task.get('endedAtMs') is not None,'expected':len(expected),'actual':len(actual),
                'missing':len(expected-actual),'unexpected':len(actual-expected)}

    def reconcile(self, bucket, task_id):
        with self.lock():
            task = self._task(task_id); self._destination(bucket,task)
            return self._reconcile(bucket,task)

    @staticmethod
    def _destination(bucket, task):
        if bucket.bucket_name != task['bucket']: raise ValueError('Bucket differs from registered task')

    def _frame(self, bucket, task, frame):
        self._require_live(task)
        key = frame['objectKey']; head = bucket.head_object(key)
        expected = {'user':task['userId'],'device':task['deviceId'],'task':task['taskId'],
                    'sha256':frame['sha256'],'captured-at':str(frame['capturedAtMs']),'display':str(frame['displayId'])}
        if task.get('sharedGroup'):
            expected['study-end'] = iso(datetime.fromtimestamp(task['plannedEndMs']/1000,timezone.utc))
        if head.content_length != frame['sizeBytes'] or header_value(head,'Content-Type') != 'image/webp' or header_value(head,'x-oss-server-side-encryption') != 'AES256':
            raise ValueError('Frame size/MIME/encryption differs')
        for k,v in expected.items():
            if header_value(head,'x-oss-meta-timelens-'+k) != v: raise ValueError('Frame metadata differs')
        assert_private_object(bucket,key)
        data = self._read(bucket,key,frame['sizeBytes']); crc = Crc64(); crc(data)
        if digest(data) != frame['sha256'] or str(crc.crc) != str(header_value(head,'x-oss-hash-crc64ecma')):
            raise ValueError('Original readback SHA/CRC differs')
        self._require_live(task)
        return data

    def verify(self, bucket, task_id, restore=False):
        with self.lock():
            task = self._task(task_id); self._destination(bucket,task)
            result = self._reconcile(bucket,task)
            if result['missing'] or result['unexpected']: raise ValueError('Reconciliation failed')
            root = private_dir(self.restores/task_id) if restore else None
            for frame in task['frames']:
                data = self._frame(bucket,task,frame)
                if root:
                    self._require_live(task)
                    target = root/(uid(frame['snapshotId'])+'.webp')
                    # Fixed managed names only; never arbitrary export paths.
                    part = root/(frame['snapshotId']+'.'+str(uuid4())+'.part')
                    fd = os.open(part,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
                    try:
                        with os.fdopen(fd,'wb') as out: out.write(data);out.flush();os.fsync(out.fileno())
                        self._require_live(task)
                        if target.is_symlink():raise ValueError('Restore file is a symlink')
                        os.replace(part,target)
                    finally:part.unlink(missing_ok=True)
            self._require_live(task); assert_private_bucket(bucket)
            result.update(verified=len(task['frames']), sizeBytes=sum(f['sizeBytes'] for f in task['frames']))
            if root: result['restoredTo'] = str(root)
            return result

    def retention(self, bucket, execute=False):
        with self.lock():
            from .cloud_ledger import synchronize
            synchronize(self,bucket.bucket_name)
            try: sync = self._sync(bucket)
            except Exception as error:
                # A failed/new inventory must not prevent known expired scopes being swept.
                sync = {'errors':[{'error':type(error).__name__}]}
            # _sync may discover an earlier-ended task in this very pass.
            # Publish its deadline and re-read max expiry before deleting anything.
            synchronize(self,bucket.bucket_name)
            devices = read_private(self.root/'devices.json', {})
            groups = read_private(self.root/'shared-groups.json', {})
            def grant_expiry(device):
                expiry = device['lastGrantExpiresMs']
                if device.get('sharedGroup'):
                    group = groups[device['sharedGroup']]
                    if any(group[k] != device[k] for k in ('userId','bucket','plannedEndMs')):
                        raise ValueError('Shared device and group identity differ')
                    expiry = max(expiry,group['lastGrantExpiresMs'])
                return expiry
            result = {'dryRun':not execute,'deleted':[],'deletedDevices':[],'deletedGroups':[],'blocked':[],'due':[], 'syncErrors':sync['errors']}
            for task in self._tasks():
                if task['bucket'] != bucket.bucket_name or now_ms() < task['deadlineMs']: continue
                result['due'].append(task['taskId'])
                expiry = grant_expiry(devices[self._device_key(task)])
                if now_ms() < expiry+GRANT_DRAIN_MS:
                    result['blocked'].append({'taskId':task['taskId'],'reason':'upload grant active or request drain pending','retryAfterMs':expiry+GRANT_DRAIN_MS})
                    continue
                if not execute: continue
                try:
                    assert_private_bucket(bucket)
                    roots = [task['prefix']+kind+'/'+task['taskId']+'/' for kind in ('tasks','manifests')]
                    for prefix in roots:
                        for key in list(object_keys(bucket,prefix)): delete_and_verify_absent(bucket,key)
                        if list(object_keys(bucket,prefix)): raise ValueError('Task objects survived deletion')
                    restore = self.restores/task['taskId']
                    if restore.is_symlink(): raise ValueError('Restore directory is a symlink')
                    if restore.exists(): shutil.rmtree(restore)
                    task.update(deleted=True,inventoryComplete=False,frames=[],publishedAtMs=0)
                    task.pop('syncError',None);self._save(task)
                    result['deleted'].append(task['taskId'])
                except Exception as error:
                    result['blocked'].append({'taskId':task['taskId'],'reason':type(error).__name__})
            # Once the approved device lifetime is over, also remove malformed/orphan
            # keys that cannot be assigned to a task. This is an exact enrolled prefix.
            if execute:
                for device in devices.values():
                    if device['bucket']!=bucket.bucket_name or now_ms()<max(device['plannedEndMs']+RETENTION_MS,grant_expiry(device)+GRANT_DRAIN_MS):continue
                    try:
                        assert_private_bucket(bucket)
                        for key in list(object_keys(bucket,device['prefix'])):delete_and_verify_absent(bucket,key)
                        if list(object_keys(bucket,device['prefix'])):raise ValueError('Device orphan cleanup incomplete')
                        result['deletedDevices'].append(device['deviceId'])
                    except Exception as error:
                        result['blocked'].append({'deviceId':device['deviceId'],'reason':type(error).__name__})
                for group in read_private(self.root/'shared-groups.json', {}).values():
                    if group['bucket'] != bucket.bucket_name or now_ms() < max(group['plannedEndMs']+RETENTION_MS,group['lastGrantExpiresMs']+GRANT_DRAIN_MS): continue
                    try:
                        assert_private_bucket(bucket)
                        for key in list(object_keys(bucket,group['prefix'])): delete_and_verify_absent(bucket,key)
                        if list(object_keys(bucket,group['prefix'])): raise ValueError('Shared orphan cleanup incomplete')
                        result['deletedGroups'].append(group['userId'])
                    except Exception as error:
                        result['blocked'].append({'sharedGroup':group['userId'],'reason':type(error).__name__})
                result['syncErrors']=[e for e in result['syncErrors'] if e.get('taskId') not in result['deleted'] and (e.get('sharedGroup') or e.get('deviceId') not in result['deletedDevices']) and e.get('sharedGroup') not in result['deletedGroups']]
            alert = self.root/'retention-alert.json'
            if result['blocked'] or result['syncErrors']:
                write_private(alert,result)
            elif execute:
                alert.unlink(missing_ok=True)
            return result


def main():
    p = argparse.ArgumentParser(description='TimeLens 管理员直传对账、原图回读、任务到期删除；Mac/Linux')
    p.add_argument('command',choices=['sync','list','reconcile','verify','restore','retention','watch'])
    p.add_argument('--registry',default=str(Path.home()/'.timelens-admin'))
    p.add_argument('--bucket');p.add_argument('--credentials-file');p.add_argument('--task')
    p.add_argument('--execute',action='store_true',help='确认执行到期删除；默认只预览')
    args = p.parse_args()
    try:
        admin = Admin(args.registry)
        if args.command == 'list':
            with admin.lock():
                print(json.dumps([{k:v for k,v in t.items() if k != 'frames'} for t in admin._tasks()]))
            return
        if not args.bucket or not args.credentials_file: p.error('--bucket and --credentials-file required')
        import oss2
        from oss2.credentials import StaticCredentialsProvider
        credentials = private_json(args.credentials_file)
        session = oss2.Session();session.session.trust_env=False
        bucket = oss2.Bucket(oss2.ProviderAuthV4(StaticCredentialsProvider(credentials['accessKeyId'],credentials['accessKeySecret'])),
                             'https://oss-cn-beijing.aliyuncs.com',args.bucket,region='cn-beijing',session=session,enable_crc=True,connect_timeout=10)
        if args.command in ('retention','watch'):
            if args.command == 'watch' and not args.execute: p.error('watch requires --execute')
            while True:
                try:
                    result = admin.retention(bucket,args.execute); print(json.dumps(result),flush=True)
                    if args.command == 'retention':
                        if result['blocked'] or result['syncErrors']: p.exit(2,'Retention needs attention; private alert retained\n')
                        return
                except Exception as error:
                    write_private(admin.root/'retention-alert.json',{'error':type(error).__name__,'atMs':now_ms()})
                    if args.command != 'watch': raise
                    print('Retention failed; private alert retained; retry in 60 seconds',flush=True)
                time.sleep(60)
        sync = admin.sync(bucket)
        if args.command == 'sync': print(json.dumps(sync));return
        uid(args.task)
        result = admin.reconcile(bucket,args.task) if args.command == 'reconcile' else admin.verify(bucket,args.task,args.command == 'restore')
        print(json.dumps(result))
    except (Exception,) as error:
        # SDK exceptions may contain AccessKey IDs or account numbers. Never print raw errors.
        p.exit(1,f'操作失败：{type(error).__name__}；请检查私有配置、任务清单和保留期。\n')


if __name__ == '__main__': main()
