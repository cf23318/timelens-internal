"""Run on the receiver host; credentials are read from private configuration only."""
import argparse
import json
import time
from uuid import UUID
from .config import Settings
from .oss_backend import make_bucket
from . import archive

def main():
    p=argparse.ArgumentParser()
    p.add_argument('command',choices=['manifest','reconcile','verify','restore','retention','retention-daemon'])
    p.add_argument('--task');p.add_argument('--survey');p.add_argument('--device')
    p.add_argument('--execute',action='store_true')
    args=p.parse_args();s=Settings.from_env()
    if args.command=='retention' and not args.execute:
        with archive.registry(s) as c:
            due=c.execute('SELECT task_id,deadline_ms,deleted FROM archives WHERE deadline_ms<=?',(archive.now_ms(s),)).fetchall()
        print(json.dumps({'dry_run':True,'due':due}));return
    b=make_bucket(s)
    if args.command in ('retention','retention-daemon'):
        while True:
            try: archive.cleanup(s,b)
            except Exception:
                if args.command!="retention-daemon":raise
                print("Retention pass failed; private alert retained, retrying",flush=True)
            if args.command=='retention':return
            time.sleep(30)
    if not args.task or str(UUID(args.task))!=args.task or not args.survey or not args.device:p.error('task, survey and device required')
    # Reuse validated device configuration; never print the token or pass it in argv.
    import os
    fd=os.open(s.tokens_file,os.O_RDONLY|os.O_NOFOLLOW)
    with os.fdopen(fd,encoding='utf-8') as f:
        st=os.fstat(f.fileno())
        if st.st_uid!=os.geteuid() or st.st_mode&0o077 or st.st_size>1024*1024:raise ValueError('Private token configuration required')
        token=json.load(f)[args.survey][args.device]
    if not s.authorized(args.survey,args.device,token):raise ValueError('Invalid credential configuration')
    result=archive.reconcile(s,b,args.task,args.survey,args.device,token)
    if args.command=='reconcile':print(json.dumps(result));return
    if result['missing'] or result['unexpected']:raise ValueError('OSS objects do not match the archive')
    if args.command=='restore':
        root=archive.restore(s,b,args.task,args.survey,args.device,token)
        print(json.dumps({'restored_to':str(root)}));return
    after='';count=0;total=0
    while True:
        page=archive.manifest(s,b,args.task,args.survey,args.device,token,after)
        for info in page['frames']:
            if args.command=='verify':archive.read_frame(s,b,args.task,args.survey,args.device,token,info['snapshot_id'])
            count+=1;total+=info['size_bytes']
        if args.command=='manifest':print(json.dumps(page))
        after=page['next_after']
        if after is None:break
    if args.command=='verify':print(json.dumps({'verified_frames':count,'bytes':total}))

if __name__=='__main__':main()
