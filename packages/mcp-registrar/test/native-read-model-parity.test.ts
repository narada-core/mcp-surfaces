import assert from 'node:assert/strict';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const tsEntrypoint=fileURLToPath(new URL('../src/main.js',import.meta.url));
const rustEntrypoint=fileURLToPath(new URL(`../../native/target/release/narada-mcp-registrar${process.platform==='win32'?'.exe':''}`,import.meta.url));
const ts=client(process.execPath,[tsEntrypoint]);const rust=client(rustEntrypoint,[]);
try{
  for(const [name,args] of [['registrar_guidance',{workflow:'materialize_carriers',tool:'registrar_carrier_validate'}],['registrar_surface_list',{}],['registrar_carrier_list',{}]] as const){
    assert.deepEqual(await rust.call(name,args),await ts.call(name,args),`${name} native parity`);
  }
  console.log('mcp-registrar native read model parity ok');
}finally{await Promise.all([ts.stop(),rust.stop()])}

function client(executable:string,args:string[]){
  const child=spawn(executable,args,{stdio:['pipe','pipe','pipe'],windowsHide:true}) as ChildProcessWithoutNullStreams;let output=Buffer.alloc(0);let stderr='';let id=0;
  child.stdout.on('data',chunk=>{output=Buffer.concat([output,chunk])});child.stderr.setEncoding('utf8');child.stderr.on('data',chunk=>{stderr+=chunk});
  return {async call(name:string,argumentsValue:unknown){const requestId=++id;const body=Buffer.from(JSON.stringify({jsonrpc:'2.0',id:requestId,method:'tools/call',params:{name,arguments:argumentsValue}}));child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);child.stdin.write(body);const response=await waitFor(requestId);assert.equal(response.error,undefined,response.error?.message??stderr);return response.result.structuredContent},stop(){child.stdin.end();return new Promise<void>(resolve=>{if(child.exitCode!==null)return resolve();child.once('exit',()=>resolve());setTimeout(()=>{child.kill();resolve()},1000).unref()})}};
  async function waitFor(requestId:number){const deadline=Date.now()+15000;while(Date.now()<deadline){const split=output.indexOf('\r\n\r\n');if(split>=0){const length=Number(output.subarray(0,split).toString('ascii').match(/Content-Length:\s*(\d+)/i)?.[1]);if(output.length>=split+4+length){const body=output.subarray(split+4,split+4+length);output=output.subarray(split+4+length);const response=JSON.parse(body.toString('utf8'));if(response.id===requestId)return response}}await new Promise(resolve=>setTimeout(resolve,10))}throw new Error(`timeout:${requestId}:${stderr}`)}
}
