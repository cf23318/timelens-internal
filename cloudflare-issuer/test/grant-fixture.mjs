// Rust contract fixture: synthetic identities/keys only, never contacts any service.
import {makeGrant} from '../src/worker.mjs';
const now=Math.floor(Date.now()/1000)*1000+Number(process.argv[4]||0);
const grant=await makeGrant({OSS_BUCKET:'test-bucket',OSS_ACCESS_KEY_ID:'SYNTHETICKEY123',OSS_ACCESS_KEY_SECRET:'synthetic-oss-secret'},
  {batchId:'pilot',scope:process.argv[3]||'device',plannedEndMs:Number(process.argv[5]||now+7*86400000)},process.argv[2],now,now+7*86400000);
process.stdout.write(JSON.stringify(grant));
