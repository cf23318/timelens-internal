"""Cloud issuer ledger bridge. Never transports images or the OSS Secret."""
import json
import re
import urllib.request
from urllib.parse import urlsplit
from .direct_admin import read_private, write_private, uid, integer, RETENTION_MS

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError('Issuer redirect rejected')

def endpoint(value):
    u=urlsplit(value)
    if u.scheme!='https' or not u.hostname or u.username or u.password or u.port or u.path not in ('','/') or u.query or u.fragment:
        raise ValueError('Invalid issuer origin')
    return value.rstrip('/')

def post(config,path,payload,token_key='syncToken'):
    token=config[token_key]
    if not isinstance(token,str) or not re.fullmatch(r'[a-f0-9]{64}',token): raise ValueError('Invalid issuer credential')
    req=urllib.request.Request(endpoint(config['issuerUrl'])+path,data=json.dumps(payload,separators=(',',':')).encode(),method='POST',headers={'Content-Type':'application/json','Authorization':'Bearer '+token})
    with urllib.request.build_opener(NoRedirect()).open(req,timeout=25) as r:
        raw=r.read(4*1024*1024+1)
        if len(raw)>4*1024*1024 or r.status not in (200,201): raise ValueError('Invalid issuer response')
        return json.loads(raw)

def synchronize(admin,bucket_name):
    """Under the administrator lock; failures MUST prevent all deletion."""
    devices=read_private(admin.root/'devices.json',{});groups=read_private(admin.root/'shared-groups.json',{})
    config=read_private(admin.root/'cloud-issuer.json',None)
    managed=any(d.get('cloudManaged') for d in devices.values()) or any(g.get('cloudManaged') for g in groups.values())
    if config is None:
        if managed: raise ValueError('Cloud issuer configuration missing; deletion blocked')
        return
    if set(config)!={'issuerUrl','syncToken'}: raise ValueError('Invalid issuer configuration')
    first=post(config,'/admin/sync',{'deadlines':[]})
    if first.get('format')!='timelens-issuer-ledger-v1' or not isinstance(first.get('devices'),list): raise ValueError('Invalid cloud ledger')
    cloud_scopes={uid(d['deviceId']):(d['userId'],d['scope']) for d in first['devices']}
    representatives={d['userId']:d['deviceId'] for d in first['devices'] if d.get('scope')=='shared'}
    deadlines={}
    for task in admin._tasks():
        if task['bucket']==bucket_name:
            # A UUID may also appear under a different shared namespace. Its
            # standalone UUID must never redirect that batch's deletion barrier.
            device=representatives.get(task['sharedGroup']) if task.get('sharedGroup') else task['deviceId'] if cloud_scopes.get(task['deviceId'])==(task['userId'],'device') else None
            if device: deadlines[device]=min(deadlines.get(device,4102444800000),task['deadlineMs'])
    # Commit monotonic deadlines and read max-issued expiry in ONE serialized
    # cloud operation. Subsequent issuance cannot outlive these deletion limits.
    ledger=post(config,'/admin/sync',{'deadlines':[{'deviceId':k,'deadlineMs':v} for k,v in deadlines.items()]})
    if ledger.get('format')!='timelens-issuer-ledger-v1' or not isinstance(ledger.get('devices'),list) or len(ledger['devices'])>2000: raise ValueError('Invalid cloud ledger')
    integer(ledger['atMs'],1);seen=set()
    for d in ledger['devices']:
        if set(d)!={'deviceId','userId','bucket','scope','plannedEndMs','deadlineMs','lastGrantExpiresMs'}: raise ValueError('Invalid cloud device')
        device=uid(d['deviceId']);user=d['userId']
        if device in seen or not isinstance(user,str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,63}',user) or d['bucket']!=bucket_name or d['scope'] not in ('device','shared'): raise ValueError('Invalid cloud scope')
        seen.add(device);plan=integer(d['plannedEndMs'],1);deadline=integer(d['deadlineMs'],1);expiry=integer(d['lastGrantExpiresMs'])
        if deadline>plan+RETENTION_MS or expiry>plan+RETENTION_MS: raise ValueError('Invalid cloud deadline')
        prefix=f'timelens/direct/v1/users/{user}/devices/';shared=d['scope']=='shared';key=f'shared:{user}:{device}' if shared else device
        entry={'deviceId':device,'userId':user,'bucket':bucket_name,'prefix':prefix+device+'/','plannedEndMs':plan}
        if shared:
            if any(x['userId']==user and not x.get('sharedGroup') for x in devices.values()): raise ValueError('Cloud scope conflicts with device grants')
            entry['sharedGroup']=user
            group={'userId':user,'bucket':bucket_name,'prefix':prefix,'plannedEndMs':plan};old=groups.get(user,{})
            if old and any(old[k]!=v for k,v in group.items()): raise ValueError('Cloud shared destination changed')
            group.update(lastGrantExpiresMs=max(expiry,old.get('lastGrantExpiresMs',0)),cloudManaged=True);groups[user]=group
        elif user in groups: raise ValueError('Cloud scope conflicts with shared grants')
        old=devices.get(key,{})
        if old and any(old[k]!=v for k,v in entry.items()): raise ValueError('Cloud destination changed')
        entry.update(lastGrantExpiresMs=max(expiry,old.get('lastGrantExpiresMs',0)),cloudManaged=True,cloudActivated=True);devices[key]=entry
    if any(d.get('cloudManaged') and d['deviceId'] not in seen and (d.get('cloudActivated') or d.get('sharedGroup') not in representatives) for d in devices.values()): raise ValueError('Cloud ledger lost enrolled devices')
    write_private(admin.root/'shared-groups.json',groups);write_private(admin.root/'devices.json',devices)
    write_private(admin.root/'cloud-ledger-sync.json',{'atMs':ledger['atMs'],'deviceCount':len(seen)})
