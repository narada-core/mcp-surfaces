import assert from 'node:assert/strict';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';

const tsEntrypoint=fileURLToPath(new URL('../src/main.js',import.meta.url));
const rustEntrypoint=fileURLToPath(new URL(`../../native/target/release/narada-mcp-registrar${process.platform==='win32'?'.exe':''}`,import.meta.url));
const ts=client(process.execPath,[tsEntrypoint]);const rust=client(rustEntrypoint,[]);
try{
  for(const [name,args] of [['registrar_guidance',{workflow:'materialize_carriers',tool:'registrar_carrier_validate'}],['registrar_surface_list',{}],['registrar_carrier_list',{}],['registrar_site_list',{}],['registrar_surface_tool_inventory_check',{observed_tools:{'agent-context':['agent_orientation_read','invented_tool']},include_ok:true}]] as const){
    assert.deepEqual(await rust.call(name,args),await ts.call(name,args),`${name} native parity`);
  }
  const sites=await ts.call('registrar_site_list',{}) as {items:Array<{site_id:string}>};
  if(sites.items[0]) assert.deepEqual(await rust.call('registrar_site_surfaces',{site_id:sites.items[0].site_id}),await ts.call('registrar_site_surfaces',{site_id:sites.items[0].site_id}),'registrar_site_surfaces native parity');
  await dynamicRegistryParity();
  console.log('mcp-registrar native read model parity ok');
}finally{await Promise.all([ts.stop(),rust.stop()])}

function client(executable:string,args:string[],environment:NodeJS.ProcessEnv={}){
  const child=spawn(executable,args,{stdio:['pipe','pipe','pipe'],windowsHide:true,env:{...process.env,...environment}}) as ChildProcessWithoutNullStreams;let output=Buffer.alloc(0);let stderr='';let id=0;
  child.stdout.on('data',chunk=>{output=Buffer.concat([output,chunk])});child.stderr.setEncoding('utf8');child.stderr.on('data',chunk=>{stderr+=chunk});
  return {async call(name:string,argumentsValue:unknown){const requestId=++id;const body=Buffer.from(JSON.stringify({jsonrpc:'2.0',id:requestId,method:'tools/call',params:{name,arguments:argumentsValue}}));child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);child.stdin.write(body);const response=await waitFor(requestId);assert.equal(response.error,undefined,response.error?.message??stderr);return response.result.structuredContent},stop(){child.stdin.end();return new Promise<void>(resolve=>{if(child.exitCode!==null)return resolve();child.once('exit',()=>resolve());setTimeout(()=>{child.kill();resolve()},1000).unref()})}};
  async function waitFor(requestId:number){const deadline=Date.now()+15000;while(Date.now()<deadline){const split=output.indexOf('\r\n\r\n');if(split>=0){const length=Number(output.subarray(0,split).toString('ascii').match(/Content-Length:\s*(\d+)/i)?.[1]);if(output.length>=split+4+length){const body=output.subarray(split+4,split+4+length);output=output.subarray(split+4+length);const response=JSON.parse(body.toString('utf8'));if(response.id===requestId)return response}}await new Promise(resolve=>setTimeout(resolve,10))}throw new Error(`timeout:${requestId}:${stderr}`)}
}

async function dynamicRegistryParity(){
  const root=mkdtempSync(join(tmpdir(),'narada-registrar-native-'));
  const siteRoot=join(root,'fixture-site');const registry=join(root,'registry.db');
  mkdirSync(join(siteRoot,'.narada'),{recursive:true});
  writeFileSync(join(siteRoot,'.narada','config.json'),JSON.stringify({surface_overrides:{'agent-context':{enabled:true,surface_implementation:'native'}}}));
  const db=new DatabaseSync(registry);db.exec('CREATE TABLE site_registry (site_id TEXT, site_root TEXT, lifecycle_status TEXT, created_at TEXT)');
  db.prepare('INSERT INTO site_registry VALUES (?, ?, ?, ?)').run('fixture-site',siteRoot,'active','2026-08-11T00:00:00Z');db.close();
  const environment={NARADA_SITE_REGISTRY_DB:registry};const tsFixture=client(process.execPath,[tsEntrypoint],environment);const rustFixture=client(rustEntrypoint,[],environment);
  try{assert.deepEqual(await rustFixture.call('registrar_site_list',{}),await tsFixture.call('registrar_site_list',{}),'registrar_site_list dynamic SQLite parity')}
  finally{await Promise.all([tsFixture.stop(),rustFixture.stop()]);rmSync(root,{recursive:true,force:true})}
}
