"""Package an explicit source-only administrator allowlist. Never include runtime data."""
from pathlib import Path
import hashlib
import json
import subprocess
import zipfile

root = Path(__file__).resolve().parents[1]
version = json.loads((root/'project/package.json').read_text())['version']
out = root/'project/src-tauri/target/distribution/admin'
out.mkdir(parents=True,exist_ok=True)
target = out/f'TimeLens-admin-tools-{version}.zip'
files = sorted((root/'upload-server/timelens_upload').glob('*.py'))
files += [root/p for p in ('upload-server/pyproject.toml','upload-server/README.md',
          'upload-server/deploy/timelens-direct-admin.plist.example','tools/create-oss-upload-grant.command',
          'docs/oss-offline-upload.md')]
with zipfile.ZipFile(target,'w',zipfile.ZIP_DEFLATED) as archive:
    for file in files:archive.write(file,file.relative_to(root))
sha=hashlib.sha256(target.read_bytes()).hexdigest()
(out/'SHA256SUMS').write_text(f'{sha}  {target.name}\n')
(out/'BUILD-INFO.json').write_text(json.dumps({'version':version,'sourceCommit':subprocess.check_output(
    ['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'files':[{'name':target.name,'sha256':sha}],
    'note':'Python source-only administrator tools; install dependencies per docs. No credentials or upload grants.'},indent=2)+'\n')
print(f'Administrator tools packaged: {target.name}')
