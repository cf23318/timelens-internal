import test from 'node:test';
import assert from 'node:assert/strict';
import {randomBytes} from 'node:crypto';
import {seal,open} from './protect-employee-build.mjs';
test('employee package round trip uses unique nonces and hides plaintext',()=>{
  const key=randomBytes(32),bytes=Buffer.from('fixture activation credential and installer');
  const a=seal(bytes,key),b=seal(bytes,key);
  assert.notDeepEqual(a,b);assert.ok(!a.includes(bytes));assert.deepEqual(open(a,key),bytes);assert.deepEqual(open(b,key),bytes);
});
test('wrong key is rejected',()=>{assert.throws(()=>open(seal(Buffer.from('test'),randomBytes(32)),randomBytes(32)));});
test('tampered ciphertext, tag and nonce are rejected',()=>{
  const key=randomBytes(32),bytes=seal(Buffer.from('test fixture'),key);
  for(const i of [26,40,bytes.length-1]){const altered=Buffer.from(bytes);altered[i]^=1;assert.throws(()=>open(altered,key));}
});
test('truncated packages and unknown formats are rejected',()=>{
  const key=randomBytes(32),bytes=seal(Buffer.from('test'),key);
  for(const n of [0,10,30,bytes.length-1])assert.throws(()=>open(bytes.subarray(0,n),key));
  const altered=Buffer.from(bytes);altered[0]^=1;assert.throws(()=>open(altered,key));
});
test('package encryption requires an exact 32-byte key',()=>{
  for(const key of [undefined,Buffer.alloc(16),Buffer.alloc(33)])assert.throws(()=>seal(Buffer.from('test'),key));
});
