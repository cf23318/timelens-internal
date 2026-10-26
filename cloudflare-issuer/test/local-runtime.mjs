// Run explicitly against wrangler dev on loopback; never uses production keys.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
const file=process.argv[2],phase=process.argv[3]||'init',base=process.env.TIMELENS_TEST_ISSUER_URL||'http://127.0.0.1:8788';
assert.match(base,/^http:\/\/127\.0\.0\.1:\d+$/);
const admin='a'.repeat(64),sync='b'.repeat(64),key='c'.repeat(64),device='33333333-3333-4333-8333-333333333333';
async function post(path,body,token){const r=await fetch(base+path,{method:'POST',headers:{'Content-Type':'application/json',...(token?{Authorization:'Bearer '+token}:{})},body:JSON.stringify(body),signal:AbortSignal.timeout(10000)});return {status:r.status,body:await r.json()};}
if(phase==='init'){
  let saved;try{saved=JSON.parse(await fs.readFile(file,'utf8'));}catch{}
  const payload=saved?Object.fromEntries(Object.entries(saved).filter(([k])=>!['deadline','oldExpiry'].includes(k))):{batchId:'runtime-pilot',scope:'device',maxDevices:2,activationCode:'d'.repeat(64),archiveEndAt:new Date(Date.now()+7*86400000).toISOString(),activationExpiresAt:new Date(Date.now()+86400000).toISOString()};
  await fs.writeFile(file,JSON.stringify(payload),{mode:0o600});
  const created=await post('/admin/batches',payload,admin);assert.equal(created.status,201);
  const repeated=await post('/admin/batches',payload,admin);assert.equal(repeated.status,201);assert.deepEqual(repeated.body,created.body);
  const activated=await post('/v1/activate',{activationCode:payload.activationCode,deviceId:device,deviceSecret:key});assert.equal(activated.status,200);
  const repeatedActivation=await post('/v1/activate',{activationCode:payload.activationCode,deviceId:device,deviceSecret:key});assert.deepEqual(repeatedActivation.body,activated.body);
  assert.equal((await post('/v1/renew',{deviceId:device},'e'.repeat(64))).status,401);
  const deadline=Date.now()+2*86400000;
  const constrained=await post('/admin/sync',{deadlines:[{deviceId:device,deadlineMs:deadline}]},sync);assert.equal(constrained.status,200);
  const renewed=await post('/v1/renew',{deviceId:device,force:true},key);assert.equal(renewed.status,200);assert.ok(Date.parse(renewed.body.grant.expiresAt)<=deadline-120000);
  const ledger=await post('/admin/sync',{deadlines:[]},sync);assert.ok(ledger.body.devices[0].lastGrantExpiresMs>=Date.parse(activated.body.grant.expiresAt));
  await fs.writeFile(file,JSON.stringify({...payload,deadline,oldExpiry:Date.parse(activated.body.grant.expiresAt)}),{mode:0o600});
  const publicBatch=await post('/admin/batches',{batchId:'runtime-open',scope:'device',maxDevices:2,activationCode:'e'.repeat(64),archiveEndAt:'2099-01-01T00:00:00Z',activationExpiresAt:'2099-01-01T00:00:00Z'},admin);assert.equal(publicBatch.status,201);
  const publicRequest={batchId:'runtime-open',participantName:'cf',deviceId:'44444444-4444-4444-8444-444444444444',deviceSecret:'f'.repeat(64),archiveEndAt:new Date(Math.floor(Date.now()/1000)*1000+7*86400000).toISOString()};
  const publicGrant=await post('/v1/register',publicRequest);assert.equal(publicGrant.status,200);assert.equal(publicGrant.body.grant.archiveEndAt,publicRequest.archiveEndAt);
  assert.deepEqual((await post('/v1/register',publicRequest)).body,publicGrant.body);
  await fs.writeFile(file+'.registration',JSON.stringify(publicRequest),{mode:0o600});
  console.log('PASS local workerd/SQLite: batch/activation/public registration retry, device deadline, authorization barrier');
}else{
  const saved=JSON.parse(await fs.readFile(file,'utf8'));const ledger=await post('/admin/sync',{deadlines:[]},sync);assert.equal(ledger.status,200);
  const d=ledger.body.devices.find(d=>d.deviceId===device);assert.ok(d);assert.equal(d.deadlineMs,saved.deadline);assert.equal(d.lastGrantExpiresMs,saved.oldExpiry);
  assert.equal((await post('/v1/renew',{deviceId:device},key)).status,200);
  const publicRequest=JSON.parse(await fs.readFile(file+'.registration','utf8'));
  const publicRetry=await post('/v1/register',publicRequest);assert.equal(publicRetry.status,200);assert.equal(publicRetry.body.grant.archiveEndAt,publicRequest.archiveEndAt);
  const publicRow=(await post('/admin/sync',{deadlines:[]},sync)).body.devices.find(d=>d.deviceId===publicRequest.deviceId);
  assert.equal(publicRow.plannedEndMs,Date.parse(publicRequest.archiveEndAt));assert.equal(publicRow.deadlineMs,Date.parse(publicRequest.archiveEndAt)+30*86400000);
  assert.equal((await post('/v1/renew',{deviceId:publicRequest.deviceId},publicRequest.deviceSecret)).status,200);
  console.log('PASS workerd restart: activation/public registration identities, per-device deadlines, renewal and expiry');
}
