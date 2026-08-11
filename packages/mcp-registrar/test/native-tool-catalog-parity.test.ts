import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { listTools } from '../src/main.js';

const executable=fileURLToPath(new URL(`../../native/target/release/narada-mcp-registrar${process.platform==='win32'?'.exe':''}`,import.meta.url));
const child=spawn(executable,[],{stdio:['pipe','pipe','pipe'],windowsHide:true});
const request=Buffer.from(JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/list',params:{}}));
child.stdin.end(`Content-Length: ${request.length}\r\n\r\n${request}`);
const stdout:Buffer[]=[];const stderr:Buffer[]=[];child.stdout.on('data',v=>stdout.push(v));child.stderr.on('data',v=>stderr.push(v));
const code=await new Promise<number|null>((resolve,reject)=>{child.once('error',reject);child.once('exit',resolve)});
assert.equal(code,0,Buffer.concat(stderr).toString('utf8'));
const message=Buffer.concat(stdout);const separator=message.indexOf('\r\n\r\n');assert.notEqual(separator,-1);
const length=Number(message.subarray(0,separator).toString('ascii').match(/Content-Length:\s*(\d+)/i)?.[1]);const body=message.subarray(separator+4);assert.equal(body.length,length);
assert.deepEqual(JSON.parse(body.toString('utf8')).result.tools,listTools());
console.log('mcp-registrar native tool catalog parity ok');
