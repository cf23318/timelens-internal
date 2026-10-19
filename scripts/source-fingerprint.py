"""Read-only source fingerprint, including new untracked source files; excludes runtime data."""
import hashlib,json,sys
from pathlib import Path
root=Path(sys.argv[1]).resolve()
skip={'.git','node_modules','target','dist','gen','.venv','.pytest_cache','__pycache__','.timelens','.timelens-internal','state','artifacts'}
files={}
for p in sorted(root.rglob('*')):
    parts=p.relative_to(root).parts
    if any(x in skip for x in parts) or p.is_symlink() or not p.is_file():continue
    if p.name in {'upload-token','device-tokens.json','server.env','.DS_Store'} or p.name.startswith('.env') or p.suffix in {'.log','.webp','.png','.jpg','.jpeg','.gif','.dmg','.exe','.msi','.sqlite','.sqlite3','.db','.age','.rar'}:continue
    files[p.relative_to(root).as_posix()]=hashlib.sha256(p.read_bytes()).hexdigest()
print(json.dumps({'source_sha256':hashlib.sha256(json.dumps(files,sort_keys=True).encode()).hexdigest(),'files':len(files)},ensure_ascii=False))
