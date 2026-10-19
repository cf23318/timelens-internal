from pathlib import Path
from types import SimpleNamespace as NS
import oss2
from oss2.utils import Crc64

class FakeBucket:
    """Memory-only OSS; no credential access or external requests."""
    def __init__(self):
        self.objects={};self.put_count=0;self.private=True;self.object_private=True;self.public_block=True;self.policy=False;self.versioning=None;self.bad_crc=False;self.after_put=None
    def get_bucket_acl(self):return NS(acl="private" if self.private else "public-read")
    def get_bucket_public_access_block(self):return NS(block_public_access=self.public_block)
    def get_bucket_versioning(self):return NS(status=self.versioning)
    def get_bucket_policy(self):
        if self.policy:return NS(policy="{}")
        raise oss2.exceptions.NoSuchBucketPolicy(404,{},b"",{"Code":"NoSuchBucketPolicy"})
    def get_object_acl(self,key):return NS(acl="private" if self.object_private else "public-read")
    def head_object(self,key):
        if key not in self.objects:raise oss2.exceptions.NotFound(404,{},b"",{"Code":"NoSuchKey"})
        data,headers=self.objects[key];crc=Crc64();crc.update(data)
        return NS(content_length=len(data),headers={**headers,"x-oss-hash-crc64ecma":str(crc.crc)})
    def put_object_from_file(self,key,filename,headers):
        assert headers["x-oss-forbid-overwrite"]=="true" and headers["x-oss-object-acl"]=="private" and headers["x-oss-server-side-encryption"]=="AES256"
        if key in self.objects:raise oss2.exceptions.OssError(409,{},b"",{"Code":"FileAlreadyExists"})
        data=Path(filename).read_bytes();self.objects[key]=(data,headers.copy());self.put_count+=1;crc=Crc64();crc.update(data)
        if self.after_put:self.after_put()
        return NS(crc=crc.crc+int(self.bad_crc))
