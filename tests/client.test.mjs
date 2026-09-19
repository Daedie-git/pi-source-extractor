import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Extractor, materialize } from '../js/index.mjs';

test('persistent client, concurrent callers, Unicode and line materialization', async () => {
  const worker = new Extractor();
  try {
    const source = '// π\nint first() { return 1; }\n';
    const [a,b] = await Promise.all([worker.extract([{path:'first.cpp',source}]),worker.extract([{path:'second.cpp',source:'int second() {}'}])]);
    assert.equal(a.results[0].extraction.units[0].name,'first');
    assert.equal(b.results[0].extraction.units[0].name,'second');
    assert.equal(materialize('first.cpp',source,a.results[0].extraction)[0].text,'int first() { return 1; }');
  } finally { worker.close(); }
  await assert.rejects(worker.extract([]),/closed/);
});

test('per-file size errors do not kill worker', async () => {
  const worker = new Extractor();
  try {
    const response = await worker.extract([{path:'large.cpp',source:' '.repeat(4*1024*1024+1)},{path:'ok.cpp',source:'int okay() {}'}]);
    assert.match(response.results[0].error,/4 MiB/);
    assert.equal(response.results[1].extraction.units[0].name,'okay');
    assert.equal((await worker.extract([])).results.length,0);
  } finally { worker.close(); }
});

test('invalid worker input gives explicit error and subsequent request works', () => {
  const result = spawnSync('target/release/pi-source-extractor',['--threads','1'],{input:'garbage\n{"id":2,"files":[]}\n',encoding:'utf8'});
  assert.equal(result.status,0);
  const [bad,good] = result.stdout.trim().split('\n').map(JSON.parse);
  assert.match(bad.error,/invalid request/);
  assert.equal(good.id,2);
});

test('spawn failure and queue limits reject without hanging', async () => {
  const missing = new Extractor({binary:'/nonexistent/pi-source-extractor'});
  await assert.rejects(missing.extract([]),/Cannot start|ENOENT/);
  missing.close();
  const worker = new Extractor({maxPending:1});
  try {
    const first = worker.extract([]);
    await assert.rejects(worker.extract([]),/queue/);
    await first;
  } finally { worker.close(); }
});

test('timeout terminates child and rejects pending work', async () => {
  const worker = new Extractor({timeoutMs:0.001});
  try { await assert.rejects(worker.extract([{path:'large.cpp',source:'int f(){}\n'.repeat(50000)}]),/timed out/); }
  finally { worker.close(); }
});

test('malformed response rejects instead of throwing from an event callback', {skip:process.platform==='win32'}, async () => {
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),'extractor-test-'));
  const binary=path.join(directory,'fake.mjs');
  fs.writeFileSync(binary,"#!/usr/bin/env node\nprocess.stdin.resume(); console.log('null');\n",{mode:0o700});
  const worker=new Extractor({binary});
  try { await assert.rejects(worker.extract([]),/invalid extractor response object/); }
  finally {worker.close();fs.rmSync(directory,{recursive:true});}
});

test('abort terminates shared pending worker work', async () => {
  const worker=new Extractor();
  const controller=new AbortController();
  try {
    const pending=worker.extract([{path:'large.cpp',source:'int f(){}\n'.repeat(50000)}],{signal:controller.signal});
    controller.abort(new Error('cancelled-test'));
    await assert.rejects(pending,/cancelled-test/);
    assert.equal((await worker.extract([])).results.length,0);
  } finally {worker.close();}
});

test('invalid source ranges cannot silently become empty or oversized excerpts', () => {
  assert.throws(()=>materialize('x.cpp','int f() {}',{units:[{name:'f',line:1,end:999}]}),/invalid extraction source range/);
});

test('idle worker does not keep its Node parent alive', () => {
  const moduleURL=new URL('../js/index.mjs',import.meta.url).href;
  const script=`import {Extractor} from ${JSON.stringify(moduleURL)}; const worker=new Extractor(); await worker.extract([]);`;
  const child=spawnSync(process.execPath,['--input-type=module','-e',script],{timeout:2000,encoding:'utf8'});
  assert.equal(child.error,undefined);
  assert.equal(child.status,0,child.stderr);
});

test('CLI default detects physical cores and overrides remain capped by file count', async () => {
  const binary=fileURLToPath(new URL(`../target/release/pi-source-extractor${process.platform==='win32'?'.exe':''}`,import.meta.url));
  const help=spawnSync(binary,['--help'],{encoding:'utf8'});
  assert.equal(help.status,0);
  const detected=Number(help.stdout.match(/physical core count \((\d+)\)/)?.[1]);
  assert.ok(detected>=1);
  const files=Array.from({length:3},(_,i)=>({path:`file-${i}.cpp`,source:`// ${'x'.repeat(40000)}\nint f${i}() {}` }));
  for(const [args,expected] of [[[],Math.min(detected,3)],[['--threads','1'],1],[['--threads','64'],3]]) {
    const result=spawnSync(binary,args,{input:JSON.stringify({id:1,files})+'\n',encoding:'utf8'});
    assert.equal(result.status,0,result.stderr);
    assert.equal(JSON.parse(result.stdout).workerThreads,expected);
  }
  const invalid=spawnSync(binary,['--threads','0'],{encoding:'utf8'});
  assert.notEqual(invalid.status,0);
  assert.match(invalid.stderr,/positive integer/);
  const worker=new Extractor({threads:64});
  try { assert.equal((await worker.extract(files)).workerThreads,3); }
  finally {worker.close();}
});

test('scope, reference qualification and constructor arity survive JSONL and materialization', async () => {
  const worker = new Extractor();
  try {
    const source = 'namespace N { struct X { X(int n, int optional=0) {} static X make() { return X{1}; } }; }';
    const { results } = await worker.extract([{ path: 'metadata.cpp', source }]);
    const units = materialize('metadata.cpp', source, results[0].extraction);
    assert.equal(units[0].qualifiedName, 'N::X::X');
    assert.equal(units[0].kind, 'constructor');
    assert.equal(units[0].minArgs, 1);
    assert.equal(units[0].maxArgs, 2);
    assert.deepEqual(units[1].references, [{ kind: 'construct', name: 'X', qualification: 'unqualified', qualifier: [], arguments: 1 }]);
    assert.equal(units[1].scope[1].kind, 'type');
  } finally { worker.close(); }
});
