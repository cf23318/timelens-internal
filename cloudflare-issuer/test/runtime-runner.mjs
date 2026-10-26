// CI/local workerd contract: ephemeral state, loopback only, synthetic keys.
import {spawn,execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {once} from 'node:events';
import net from 'node:net';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
const run=promisify(execFile),root=fileURLToPath(new URL('..',import.meta.url));
const temp=await fs.mkdtemp(path.join(os.tmpdir(),'timelens-issuer-test-'));
const socket=net.createServer();socket.listen(0,'127.0.0.1');await once(socket,'listening');const port=socket.address().port;await new Promise(resolve=>socket.close(resolve));
const base=`http://127.0.0.1:${port}`,env={...process.env,WRANGLER_SEND_METRICS:'false',CI:'true',TIMELENS_TEST_ISSUER_URL:base};
let child,logs='';
async function start(){
  child=spawn(process.execPath,['node_modules/wrangler/wrangler-dist/cli.js','dev','--local','--ip','127.0.0.1','--port',String(port),'--persist-to',path.join(temp,'state'),
    '--var','OSS_BUCKET:test-bucket','--var','OSS_ACCESS_KEY_ID:SYNTHETICKEY123','--var','OSS_ACCESS_KEY_SECRET:synthetic-oss-secret',
    '--var','ISSUER_ADMIN_TOKEN:'+'a'.repeat(64),'--var','ISSUER_SYNC_TOKEN:'+'b'.repeat(64),'--var','PUBLIC_ISSUER_URL:https://issuer.example.com/','--var','OPEN_REGISTRATION_BATCH:runtime-open'],{cwd:root,env,stdio:['ignore','pipe','pipe']});
  child.stdout.on('data',b=>{logs=(logs+b).slice(-8192);});child.stderr.on('data',b=>{logs=(logs+b).slice(-8192);});
  for(let i=0;i<100;i++){
    if(child.exitCode!==null)throw Error('Local runtime exited: '+logs);
    try{const r=await fetch(base+'/admin/sync',{method:'POST',headers:{'Content-Type':'application/json',Authorization:'Bearer '+'b'.repeat(64)},body:'{"deadlines":[]}',signal:AbortSignal.timeout(1000)});if(r.status===200)return;}catch{}
    await new Promise(r=>setTimeout(r,100));
  }
  throw Error('Local runtime did not start: '+logs);
}
async function stop(){if(!child || child.exitCode!==null)return;const p=once(child,'exit');child.kill('SIGINT');const timer=setTimeout(()=>child.kill('SIGKILL'),5000);try{await p;}finally{clearTimeout(timer);}}
try{
  await start();const first=await run(process.execPath,['test/local-runtime.mjs',path.join(temp,'fixture.private.json'),'init'],{cwd:root,env});process.stdout.write(first.stdout);
  await stop();await start();const second=await run(process.execPath,['test/local-runtime.mjs',path.join(temp,'fixture.private.json'),'restart'],{cwd:root,env});process.stdout.write(second.stdout);
}finally{await stop();await fs.rm(temp,{recursive:true,force:true});}
