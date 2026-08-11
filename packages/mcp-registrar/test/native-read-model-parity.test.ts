import assert from 'node:assert/strict';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';
import { payloadCreate } from '@narada-core/mcp-transport';

const tsEntrypoint=fileURLToPath(new URL('../src/main.js',import.meta.url));
const rustEntrypoint=fileURLToPath(new URL(`../../native/target/release/narada-mcp-registrar${process.platform==='win32'?'.exe':''}`,import.meta.url));
const ts=client(process.execPath,[tsEntrypoint]);const rust=client(rustEntrypoint,[]);
try{
  for(const [name,args] of [['registrar_guidance',{workflow:'materialize_carriers',tool:'registrar_carrier_validate'}],['registrar_surface_list',{}],['registrar_carrier_list',{}],['registrar_site_list',{}],['registrar_surface_tool_inventory_check',{observed_tools:{'agent-context':['agent_orientation_read','invented_tool']},include_ok:true}]] as const){
    assert.deepEqual(await rust.call(name,args),await ts.call(name,args),`${name} native parity`);
  }
  const sites=await ts.call('registrar_site_list',{}) as {items:Array<{site_id:string}>};
  const carriers=await ts.call('registrar_carrier_list',{}) as {items:Array<{carrier_id:string}>};
  for(const carrier of carriers.items) for(const include_ok of [false,true]) assert.deepEqual(await rust.call('registrar_carrier_validate',{carrier_id:carrier.carrier_id,include_ok}),await ts.call('registrar_carrier_validate',{carrier_id:carrier.carrier_id,include_ok}),`registrar_carrier_validate ${carrier.carrier_id} include_ok=${include_ok} parity`);
  for(const carrier of carriers.items) assert.deepEqual(await rust.call('registrar_carrier_diff',{carrier_id:carrier.carrier_id}),await ts.call('registrar_carrier_diff',{carrier_id:carrier.carrier_id}),`registrar_carrier_diff ${carrier.carrier_id} parity`);
  if(sites.items[0]) assert.deepEqual(await rust.call('registrar_site_surfaces',{site_id:sites.items[0].site_id}),await ts.call('registrar_site_surfaces',{site_id:sites.items[0].site_id}),'registrar_site_surfaces native parity');
  if(sites.items[0]) assert.deepEqual(await rust.call('registrar_site_mcp_fabric_validate',{site_id:sites.items[0].site_id}),await ts.call('registrar_site_mcp_fabric_validate',{site_id:sites.items[0].site_id}),'registrar_site_mcp_fabric_validate live parity');
  if(sites.items[0]) assert.deepEqual(await rust.call('registrar_site_mcp_fabric_validate',{site_id:sites.items[0].site_id,include_ok:true}),await ts.call('registrar_site_mcp_fabric_validate',{site_id:sites.items[0].site_id,include_ok:true}),'registrar_site_mcp_fabric_validate include-ok parity');
  if(sites.items[0]){const rustRegistry=await rust.call('registrar_site_surface_registry_sync',{site_id:sites.items[0].site_id,dry_run:true}) as any;const tsRegistry=await ts.call('registrar_site_surface_registry_sync',{site_id:sites.items[0].site_id,dry_run:true}) as any;delete rustRegistry.registry.generated_at;delete tsRegistry.registry.generated_at;assert.deepEqual(rustRegistry,tsRegistry,'registrar_site_surface_registry_sync dry-run parity')}
  for(const surface_id of ['agent-context','fixture.local']) assert.deepEqual(await rust.call('registrar_surface_usage',{surface_id}),await ts.call('registrar_surface_usage',{surface_id}),`registrar_surface_usage ${surface_id} parity`);
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
  writeFileSync(join(siteRoot,'.narada','config.json'),JSON.stringify({surface_overrides:{'agent-context':{enabled:true,surface_implementation:'native'},'quota-meter':{enabled:false}},structural_config:{agent_execution_policy:{allowed_mcp_entrypoints:[{surface_id:'fixture.local',command:'fixture',path:'fixture.exe'}]}}}));
  const db=new DatabaseSync(registry);db.exec('CREATE TABLE site_registry (site_id TEXT, site_root TEXT, lifecycle_status TEXT, created_at TEXT)');
  db.prepare('INSERT INTO site_registry VALUES (?, ?, ?, ?)').run('andrey-user',join(process.env.USERPROFILE??'', 'Narada'),'active','2026-08-10T00:00:00Z');
  db.prepare('INSERT INTO site_registry VALUES (?, ?, ?, ?)').run('fixture-site',siteRoot,'active','2026-08-11T00:00:00Z');db.close();
  mkdirSync(join(siteRoot,'.narada','capabilities'),{recursive:true});
  writeFileSync(join(siteRoot,'.narada','capabilities','mcp-surfaces.json'),JSON.stringify({surfaces:[{surface_id:'git',server_name:'fixture-git',catalog_surface_id:'git',registered_live_tools:['git_status'],tool_contract:{read_only_tools:[],mutating_tools:[],refused_tools:[]}}]}));
  mkdirSync(join(siteRoot,'.ai','mcp'),{recursive:true});
  writeFileSync(join(siteRoot,'.ai','mcp','fixture-git.json'),JSON.stringify({mcpServers:{'narada-fixture-site-git':{surface_id:'git',command:'fixture',args:[]}}}));
  const environment={NARADA_SITE_REGISTRY_DB:registry};const tsFixture=client(process.execPath,[tsEntrypoint],environment);const rustFixture=client(rustEntrypoint,[],environment);
  try{
    assert.deepEqual(await rustFixture.call('registrar_site_list',{}),await tsFixture.call('registrar_site_list',{}),'registrar_site_list dynamic SQLite parity');
    assert.deepEqual(await rustFixture.call('registrar_site_mcp_fabric_validate',{site_id:'fixture-site'}),await tsFixture.call('registrar_site_mcp_fabric_validate',{site_id:'fixture-site'}),'registrar_site_mcp_fabric_validate invalid fixture parity');
    assert.deepEqual(await rustFixture.call('registrar_site_output_reader_closure_check',{site_root:siteRoot,include_ok:true}),await tsFixture.call('registrar_site_output_reader_closure_check',{site_root:siteRoot,include_ok:true}),'registrar_site_output_reader_closure_check parity');
    for(const surface_id of ['git','fixture.local']) assert.deepEqual(await rustFixture.call('registrar_surface_usage',{surface_id}),await tsFixture.call('registrar_surface_usage',{surface_id}),`registrar_surface_usage dynamic ${surface_id} parity`);
    const tsSync=await tsFixture.call('registrar_site_surface_registry_sync',{site_id:'fixture-site'});const tsRegistry=JSON.parse(readFileSync(join(siteRoot,'.narada','capabilities','mcp-surfaces.json'),'utf8'));
    const rustSync=await rustFixture.call('registrar_site_surface_registry_sync',{site_id:'fixture-site'});const rustRegistry=JSON.parse(readFileSync(join(siteRoot,'.narada','capabilities','mcp-surfaces.json'),'utf8'));
    delete tsRegistry.generated_at;delete rustRegistry.generated_at;assert.deepEqual(rustSync,tsSync,'registrar_site_surface_registry_sync write result parity');assert.deepEqual(rustRegistry,tsRegistry,'registrar_site_surface_registry_sync write artifact parity');
    const observed_tools=Object.fromEntries(rustRegistry.surfaces.map((surface:any)=>[surface.server_name,surface.registered_live_tools]));
    const observed_read_only_tools=Object.fromEntries(rustRegistry.surfaces.map((surface:any)=>[surface.server_name,surface.tool_contract.read_only_tools]));
    const observed_mutating_tools=Object.fromEntries(rustRegistry.surfaces.map((surface:any)=>[surface.server_name,surface.tool_contract.mutating_tools]));
    const observation:any=payloadCreate({siteRoot,args:{payload_id:'site-tools-fixture',created_by:'mcp-loader-mcp',payload:{schema:'narada.mcp_loader.site_tool_inventory_check.v1',status:'ok',site_root:siteRoot,observed_at:'2026-08-11T00:00:00.000Z',observed_tools,observed_read_only_tools,observed_mutating_tools}}});
    for(const include_ok of [false,true]) assert.deepEqual(await rustFixture.call('registrar_site_registry_conformance_check',{site_id:'fixture-site',observation_ref:observation.ref,include_ok}),await tsFixture.call('registrar_site_registry_conformance_check',{site_id:'fixture-site',observation_ref:observation.ref,include_ok}),`registrar_site_registry_conformance_check include_ok=${include_ok} parity`);
    const drifted=structuredClone(rustRegistry);drifted.schema='invalid';drifted.site_id='wrong-site';drifted.generated_by='other';drifted.generation_policy.mode='other';drifted.generated_at='invalid';drifted.surfaces[0].registered_live_tools=[];drifted.surfaces[0].tool_contract.read_only_tools=[];drifted.surfaces.push({...drifted.surfaces[0],server_name:'ghost-server'});writeFileSync(join(siteRoot,'.narada','capabilities','mcp-surfaces.json'),JSON.stringify(drifted));
    assert.deepEqual(await rustFixture.call('registrar_site_registry_conformance_check',{site_id:'fixture-site',observation_ref:observation.ref}),await tsFixture.call('registrar_site_registry_conformance_check',{site_id:'fixture-site',observation_ref:observation.ref}),'registrar_site_registry_conformance_check drift parity');writeFileSync(join(siteRoot,'.narada','capabilities','mcp-surfaces.json'),JSON.stringify(rustRegistry));
    const diagnosticRegistry=structuredClone(rustRegistry);const diagnosticSurface=diagnosticRegistry.surfaces[0];const duplicateTool=diagnosticSurface.registered_live_tools[0];const mutatingTool=diagnosticSurface.tool_contract.mutating_tools[0];diagnosticSurface.registered_live_tools.push(duplicateTool);diagnosticSurface.tool_contract.read_only_tools.push(duplicateTool,duplicateTool,mutatingTool);diagnosticSurface.tool_contract.refused_tools.push(duplicateTool);diagnosticSurface.client_config={drift:true};writeFileSync(join(siteRoot,'.narada','capabilities','mcp-surfaces.json'),JSON.stringify(diagnosticRegistry));
    const diagnosticObservation:any=payloadCreate({siteRoot,args:{payload_id:'site-tools-diagnostics',created_by:'mcp-loader-mcp',payload:{schema:'narada.mcp_loader.site_tool_inventory_check.v1',status:'drift',site_root:siteRoot,observed_at:'2026-08-11T00:00:01.000Z',observed_tools:{[diagnosticSurface.server_name]:[duplicateTool,duplicateTool]},observed_read_only_tools:{[diagnosticSurface.server_name]:[duplicateTool,duplicateTool,mutatingTool]},observed_mutating_tools:{[diagnosticSurface.server_name]:[mutatingTool,mutatingTool]}}}});
    assert.deepEqual(await rustFixture.call('registrar_site_registry_conformance_check',{site_id:'fixture-site',observation_ref:diagnosticObservation.ref}),await tsFixture.call('registrar_site_registry_conformance_check',{site_id:'fixture-site',observation_ref:diagnosticObservation.ref}),'registrar_site_registry_conformance_check diagnostic parity');writeFileSync(join(siteRoot,'.narada','capabilities','mcp-surfaces.json'),JSON.stringify(rustRegistry));
    const boundFile=join(siteRoot,'.ai','mcp','narada-fixture-site-structured-command-mcp.json');
    const tsBind=await tsFixture.call('registrar_site_bind',{site_id:'fixture-site',surface_id:'structured-command'});const tsBound=JSON.parse(readFileSync(boundFile,'utf8'));const tsBoundRegistry=JSON.parse(readFileSync(join(siteRoot,'.narada','capabilities','mcp-surfaces.json'),'utf8'));rmSync(boundFile);
    const rustBind=await rustFixture.call('registrar_site_bind',{site_id:'fixture-site',surface_id:'structured-command'});const rustBound=JSON.parse(readFileSync(boundFile,'utf8'));const rustBoundRegistry=JSON.parse(readFileSync(join(siteRoot,'.narada','capabilities','mcp-surfaces.json'),'utf8'));delete tsBoundRegistry.generated_at;delete rustBoundRegistry.generated_at;assert.deepEqual(rustBind,tsBind,'registrar_site_bind result parity');assert.deepEqual(rustBound,tsBound,'registrar_site_bind artifact parity');assert.deepEqual(rustBoundRegistry,tsBoundRegistry,'registrar_site_bind registry parity');
    const tsUnbind=await tsFixture.call('registrar_site_unbind',{site_id:'fixture-site',surface_id:'structured-command'});await rustFixture.call('registrar_site_bind',{site_id:'fixture-site',surface_id:'structured-command'});const rustUnbind=await rustFixture.call('registrar_site_unbind',{site_id:'fixture-site',surface_id:'structured-command'});assert.deepEqual(rustUnbind,tsUnbind,'registrar_site_unbind parity');
    assert.deepEqual(await rustFixture.call('registrar_site_bind',{site_id:'fixture-site',surface_id:'quota-meter'}),await tsFixture.call('registrar_site_bind',{site_id:'fixture-site',surface_id:'quota-meter'}),'registrar_site_bind disabled refusal parity');
    const aggregate=join(siteRoot,'.ai','mcp','fixture-site-mcp.json');writeFileSync(aggregate,JSON.stringify({mcpServers:{}}));assert.deepEqual(await rustFixture.call('registrar_site_bind',{site_id:'fixture-site',surface_id:'structured-command'}),await tsFixture.call('registrar_site_bind',{site_id:'fixture-site',surface_id:'structured-command'}),'registrar_site_bind aggregate refusal parity');rmSync(aggregate);
  }
  finally{await Promise.all([tsFixture.stop(),rustFixture.stop()]);rmSync(root,{recursive:true,force:true})}
}
