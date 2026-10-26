import {test} from 'node:test';
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {Issuer,makeGrant} from '../src/worker.mjs';
const DAY=86400000, device='11111111-1111-4111-8111-111111111111', other='22222222-2222-4222-8222-222222222222';
const env={OSS_BUCKET:'test-bucket',OSS_ACCESS_KEY_ID:'SYNTHETICKEY123',OSS_ACCESS_KEY_SECRET:'synthetic-oss-secret',ISSUER_ADMIN_TOKEN:'a'.repeat(64),ISSUER_SYNC_TOKEN:'b'.repeat(64),PUBLIC_ISSUER_URL:'https://issuer.example.com/'};
class Storage {
  map=new Map();
  async get(k){return structuredClone(this.map.get(k));}
  async put(k,v){if(typeof k==='object'){for(const [a,b] of Object.entries(k))this.map.set(a,structuredClone(b));}else this.map.set(k,structuredClone(v));}
  async list({prefix,limit}){return new Map([...this.map].filter(([k])=>k.startsWith(prefix)).slice(0,limit).map(([k,v])=>[k,structuredClone(v)]));}
}
function fixture(){
  const storage=new Storage();let queue=Promise.resolve();
  const issuer=new Issuer({storage,blockConcurrencyWhile(fn){const p=queue.then(fn);queue=p.catch(()=>{});return p;}},env);
  const call=async(path,payload,token)=>{const r=await issuer.fetch(new Request('https://issuer.example.com'+path,{method:'POST',headers:{'Content-Type':'application/json',...(token?{Authorization:'Bearer '+token}:{})},body:JSON.stringify(payload)}));return {status:r.status,body:await r.json(),headers:r.headers};};
  const batch=async(scope='device',maxDevices=2,id='pilot')=>(await call('/admin/batches',{batchId:id,scope,maxDevices,archiveEndAt:new Date(Date.now()+7*DAY).toISOString(),activationExpiresAt:new Date(Date.now()+DAY).toISOString()},env.ISSUER_ADMIN_TOKEN)).body;
  const activate=(a,id=device,key='c'.repeat(64))=>call('/v1/activate',{activationCode:a.activationCode,deviceId:id,deviceSecret:key});
  return {storage,call,batch,activate};
}
test('Worker HMAC grants match Python signer byte for byte',()=>{
  return (async()=>{
    const now=Math.floor(Date.now()/1000)*1000,plan=now+7*DAY,b={batchId:'pilot',scope:'device',plannedEndMs:plan};
    const grant=await makeGrant(env,b,device,now,plan);
    const code="import sys,json;from datetime import datetime,timedelta,timezone;from timelens_upload.oss_grant import make_grant;n=int(sys.argv[1]);t=datetime.fromtimestamp(n/1000,timezone.utc);print(json.dumps(make_grant(sys.argv[2],'pilot','test-bucket','SYNTHETICKEY123','synthetic-oss-secret',now=t,study_end=(t+timedelta(days=7)).isoformat())))";
    const actual=JSON.parse(execFileSync(process.env.TIMELENS_TEST_PYTHON||'python3',['-c',code,String(now),device],{env:{...process.env,PYTHONPATH:new URL('../../upload-server',import.meta.url).pathname}}));
    assert.deepEqual(grant,actual);
  })();
});
test('admin and sync permissions are separate; no anonymous issuance',async()=>{
  const f=fixture();assert.equal((await f.call('/admin/batches',{},env.ISSUER_SYNC_TOKEN)).status,401);
  assert.equal((await f.call('/admin/sync',{deadlines:[]})).status,401);
  const a=await f.batch();const r=await f.activate(a);assert.equal(r.status,200);assert.equal(r.headers.get('cache-control'),'no-store');
  assert.equal((await f.call('/v1/renew',{deviceId:device},'d'.repeat(64))).status,401);
  assert.equal((await f.call('/v1/renew',{deviceId:device,prefix:'other'},'c'.repeat(64))).status,400);
});
test('activation retry is idempotent and cannot steal an existing fixed ID',async()=>{
  const f=fixture(),a=await f.batch('device',1),r=await f.activate(a);
  assert.deepEqual((await f.activate(a)).body,r.body);
  assert.equal((await f.activate(a,device,'d'.repeat(64))).status,409);
  assert.equal((await f.activate(a,other)).status,403);
  const b=await f.storage.get('batch:pilot');assert.equal(b.count,1);
});
test('expired activation rejects new devices while established device keeps renewing',async()=>{
  const f=fixture(),a=await f.batch();await f.activate(a);
  const b=await f.storage.get('batch:pilot');b.activationEndMs=Date.now()-1000;await f.storage.put('batch:pilot',b);
  assert.equal((await f.activate(a,other)).status,403);
  assert.equal((await f.call('/v1/renew',{deviceId:device},'c'.repeat(64))).status,200);
});
test('cached grant, renewal, expiry registration and revocation',async t=>{
  const f=fixture(),a=await f.batch(),r=await f.activate(a),now=Date.now();
  assert.deepEqual((await f.call('/v1/renew',{deviceId:device},'c'.repeat(64))).body,r.body);
  t.mock.method(Date,'now',()=>now+6*DAY+1000);
  const renewed=await f.call('/v1/renew',{deviceId:device},'c'.repeat(64));assert.equal(renewed.status,200);
  assert.ok(Date.parse(renewed.body.grant.expiresAt)>Date.parse(r.body.grant.expiresAt));
  assert.equal(renewed.body.grant.archiveEndAt,r.body.grant.archiveEndAt);
  const ledger=(await f.call('/admin/sync',{deadlines:[]},env.ISSUER_SYNC_TOKEN)).body;
  assert.equal(ledger.devices[0].lastGrantExpiresMs,Date.parse(renewed.body.grant.expiresAt));
  assert.ok(!JSON.stringify(ledger).includes('deviceSecret'));assert.ok(!JSON.stringify(ledger).includes('signature'));
  await f.call('/admin/device',{deviceId:device,disabled:true},env.ISSUER_ADMIN_TOKEN);
  assert.equal((await f.call('/v1/renew',{deviceId:device},'c'.repeat(64))).status,403);
});
test('deadline barrier is monotonic and concurrent grants never extend past it',async t=>{
  const f=fixture(),a=await f.batch();await f.activate(a);
  const now=Date.now();t.mock.method(Date,'now',()=>now+61000);
  const deadline=Date.now()+2*DAY;
  const [sync,renew]=await Promise.all([f.call('/admin/sync',{deadlines:[{deviceId:device,deadlineMs:deadline}]},env.ISSUER_SYNC_TOKEN),f.call('/v1/renew',{deviceId:device,force:true},'c'.repeat(64))]);
  assert.equal(sync.status,200);assert.equal(renew.status,200);assert.ok(Date.parse(renew.body.grant.expiresAt)<=deadline-120000);
  const later=await f.call('/admin/sync',{deadlines:[{deviceId:device,deadlineMs:deadline+DAY}]},env.ISSUER_SYNC_TOKEN);assert.equal(later.body.devices[0].deadlineMs,deadline);
  t.mock.method(Date,'now',()=>deadline);
  assert.equal((await f.call('/v1/renew',{deviceId:device},'c'.repeat(64))).status,410);
});
test('shared earliest deadline constrains all devices and later activation',async t=>{
  const f=fixture(),a=await f.batch('shared');await f.activate(a);
  const deadline=Date.now()+DAY;await f.call('/admin/sync',{deadlines:[{deviceId:device,deadlineMs:deadline}]},env.ISSUER_SYNC_TOKEN);
  const r=await f.activate(a,other);assert.equal(r.status,200);assert.equal(r.body.grant.deviceId,'*');assert.ok(Date.parse(r.body.grant.expiresAt)<=deadline-120000);
});
test('unknown deadline and malformed requests fail without returning usable policies',async()=>{
  const f=fixture();assert.equal((await f.call('/admin/sync',{deadlines:[{deviceId:device,deadlineMs:Date.now()}]},env.ISSUER_SYNC_TOKEN)).status,409);
  assert.equal((await f.call('/v1/activate',{activationCode:'bad',deviceId:device,deviceSecret:'c'.repeat(64)})).status,400);
  const r=await f.call('/v1/activate',{screenshot:'private-content'});assert.equal(r.status,400);assert.deepEqual(r.body,{error:'RequestRejected'});
});
test('storage failure cannot export an unregistered grant',async()=>{
  const f=fixture(),a=await f.batch();const put=f.storage.put.bind(f.storage);
  f.storage.put=async(k,v)=>{if(typeof k==='string' && k.startsWith('device:') && v.grant)throw Error('disk failure');return put(k,v);};
  const r=await f.activate(a);assert.equal(r.status,503);assert.ok(!r.body.grant);
});
test('administrator can retry creating the same batch after a lost reply',async()=>{
  const f=fixture(),payload={batchId:'pilot',scope:'device',maxDevices:3,activationCode:'e'.repeat(64),archiveEndAt:new Date(Date.now()+7*DAY).toISOString(),activationExpiresAt:new Date(Date.now()+DAY).toISOString()};
  const first=await f.call('/admin/batches',payload,env.ISSUER_ADMIN_TOKEN);
  assert.equal(first.status,201);assert.deepEqual((await f.call('/admin/batches',payload,env.ISSUER_ADMIN_TOKEN)).body,first.body);
  assert.equal((await f.call('/admin/batches',{...payload,maxDevices:4},env.ISSUER_ADMIN_TOKEN)).status,409);
});
test('sync accepts the full service device limit and rejects overflow',async()=>{
  const f=fixture(),a=await f.batch();await f.activate(a);
  const row={deviceId:device,deadlineMs:Date.now()+DAY};
  assert.equal((await f.call('/admin/sync',{deadlines:Array(2000).fill(row)},env.ISSUER_SYNC_TOKEN)).status,200);
  assert.equal((await f.call('/admin/sync',{deadlines:Array(2001).fill(row)},env.ISSUER_SYNC_TOKEN)).status,400);
});
