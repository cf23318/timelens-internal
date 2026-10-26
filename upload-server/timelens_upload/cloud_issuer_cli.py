"""Administrator creates a batch once; employees activate without OSS keys."""
import argparse
import secrets
from pathlib import Path
from .direct_admin import read_private, write_private
from .cloud_ledger import post, endpoint

def main():
    p=argparse.ArgumentParser(description='创建Cloudflare自动续期批次；只签发配置，不上传图片')
    p.add_argument('--credentials-file',required=True,help='0600 JSON：issuerUrl、adminToken；不要在命令行传token')
    p.add_argument('--batch-id',required=True);p.add_argument('--study-end',required=True);p.add_argument('--activation-end',required=True)
    p.add_argument('--max-devices',type=int,default=100);p.add_argument('--scope',choices=['device','shared'],default='device');p.add_argument('--output',required=True)
    args=p.parse_args()
    try:
        config=read_private(Path(args.credentials_file))
        if set(config)!={'issuerUrl','adminToken'}: raise ValueError('Invalid admin credential file')
        target=Path(args.output)
        if target.exists() or target.is_symlink(): raise ValueError('Output already exists')
        payload={'batchId':args.batch_id,'archiveEndAt':args.study_end,'activationExpiresAt':args.activation_end,'scope':args.scope,'maxDevices':args.max_devices}
        pending=target.with_name(target.name+'.pending.private.json')
        saved=read_private(pending,None)
        if saved:
            if saved['issuerUrl']!=config['issuerUrl'] or {k:v for k,v in saved['payload'].items() if k!='activationCode'}!=payload: raise ValueError('Pending request differs')
            payload=saved['payload']
        else:
            payload['activationCode']=secrets.token_hex(32)
            write_private(pending,{'issuerUrl':config['issuerUrl'],'payload':payload})
        grant=post(config,'/admin/batches',payload,'adminToken')
        if set(grant)!={'format','issuerUrl','activationCode','batchId','scope','bucket','archiveEndAt'} or grant['format']!='timelens-cloudflare-activation-v1' or endpoint(grant['issuerUrl'])!=endpoint(config['issuerUrl']) or grant['batchId']!=args.batch_id or grant['scope']!=args.scope: raise ValueError('Invalid activation response')
        # A local export failure leaves the cloud batch allocated. Do not silently
        # create a replacement batch with a different retention deadline.
        from .oss_grant import write_grant
        write_grant(target,grant)
        pending.unlink()
        print(f'已生成激活配置：{target.resolve()}；此文件可供本批次员工首次导入，勿公开发布。')
    except Exception as error:
        p.exit(1,f'操作失败：{type(error).__name__}；未显示密钥或授权内容。若云端创建已成功，请核对批次后处理。\n')

if __name__=='__main__':main()
