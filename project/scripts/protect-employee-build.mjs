// Employee builds contain a bootstrap credential. Only authenticated ciphertext
// is uploaded as an artifact; this tool never handles OSS administrator keys.
import { createCipheriv, createDecipheriv, randomBytes } from 'node:crypto';
import { readFile, writeFile, lstat } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

const MAGIC = Buffer.from('TIMELENS_EMPLOYEE_BUILD_V1\n');
function checkKey(key) { if (!Buffer.isBuffer(key) || key.length !== 32) throw Error('Invalid package key'); }
export function seal(bytes, key) {
  checkKey(key);
  const nonce=randomBytes(12),cipher=createCipheriv('aes-256-gcm',key,nonce);
  cipher.setAAD(MAGIC);
  const ciphertext=Buffer.concat([cipher.update(bytes),cipher.final()]);
  return Buffer.concat([MAGIC,nonce,cipher.getAuthTag(),ciphertext]);
}
export function open(bytes, key) {
  checkKey(key);
  if(bytes.length<=MAGIC.length+28 || !bytes.subarray(0,MAGIC.length).equals(MAGIC))throw Error('Invalid protected package');
  const nonce=bytes.subarray(MAGIC.length,MAGIC.length+12),tag=bytes.subarray(MAGIC.length+12,MAGIC.length+28);
  const decipher=createDecipheriv('aes-256-gcm',key,nonce);decipher.setAAD(MAGIC);decipher.setAuthTag(tag);
  return Buffer.concat([decipher.update(bytes.subarray(MAGIC.length+28)),decipher.final()]);
}
async function main() {
  const [mode,input,output]=process.argv.slice(2);
  if(!['seal','open'].includes(mode)||!input||!output||process.argv.length!==5)throw Error('Expected seal/open input output');
  const encoded=process.env.TIMELENS_BUILD_ENCRYPTION_KEY;
  if(!/^[a-f0-9]{64}$/.test(encoded||''))throw Error('Missing 32-byte package key');
  const info=await lstat(input);
  if(!info.isFile()||info.size>512*1024*1024)throw Error('Expected a regular package up to 512 MiB');
  const key=Buffer.from(encoded,'hex');
  try {await writeFile(output,(mode==='seal'?seal:open)(await readFile(input),key),{flag:'wx',mode:0o600});}
  finally{key.fill(0);}
  console.log(mode==='seal'?'Employee package encrypted':'Employee package authenticated and decrypted');
}
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href) {
  main().catch(()=>{console.error('Protected employee package operation failed; no key or credential was printed.');process.exitCode=1;});
}
