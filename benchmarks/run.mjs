import fs from 'node:fs';
import os from 'node:os';
import crypto from 'node:crypto';
import { performance } from 'node:perf_hooks';
import { Extractor, materialize } from '../js/index.mjs';

const manifest = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (manifest.singles?.length !== 3 || manifest.batch?.length !== 8) throw Error('manifest requires three singles and eight batch paths');
const hash = x => crypto.createHash('sha256').update(x).digest('hex');
const inputs = [...new Set([...manifest.singles, ...manifest.batch])].map(path => {
  const bytes = fs.readFileSync(path);
  return { path, bytes: bytes.length, hash: hash(bytes) };
});
const result = { environment: { node: process.version, cpu: os.cpus()[0]?.model, cpus: os.availableParallelism(), platform: os.platform() }, protocol: { singleRepetitions: 10, coldRepetitions: 10, batchRepetitions: 5, stressRepetitions: 20, warmups: 'startup observations retained separately', cache: 'warm filesystem; sources hashed before timing' }, inputs, samples: [] };

async function measure(condition, paths, worker, repeat) {
  const start = performance.now();
  let response, units, error;
  try {
    const files = paths.map(path => ({ path, source: fs.readFileSync(path, 'utf8') }));
    response = await worker.extract(files);
    units = response.results.map((r,i) => {
      if (r.error) throw Error(r.error);
      return materialize(r.path, files[i].source, r.extraction);
    });
  } catch(e) { error = String(e); }
  const ms = performance.now() - start;
  const sample = { condition, repeat, paths, ms, error: error ?? null, parallel: response?.parallel };
  if (!error) { sample.outputHashes = units.map(x => hash(JSON.stringify(x))); sample.diagnostics = response.results.map(r => ({parseHasError:r.extraction.parseHasError,omitted:r.extraction.omitted,recovered:r.extraction.recovered})); }
  result.samples.push(sample);
}

for(let i=0;i<10;i++) {
  const worker = new Extractor({threads:1});
  try { await measure('cold', [manifest.singles[0]], worker, i); } finally { worker.close(); }
}
const worker = new Extractor({threads:4});
try {
  await measure('single-startup', [manifest.singles[0]], worker, 0);
  for(let i=0;i<10;i++) for(let j=0;j<3;j++) await measure('warm-single', [manifest.singles[(i+j)%3]], worker, i);
  const largest = manifest.singles.reduce((a,b) => fs.statSync(a).size >= fs.statSync(b).size ? a : b);
  for(let i=0;i<20;i++) await measure('stress', [largest], worker, i);
} finally { worker.close(); }
const serial = new Extractor({threads:1}), parallel = new Extractor({threads:4});
try {
  await measure('batch-serial-startup', manifest.batch, serial, 0);
  await measure('batch-parallel-startup', manifest.batch, parallel, 0);
  for(let i=0;i<5;i++) {
    const arms=i%2 ? [['batch-parallel',parallel],['batch-serial',serial]] : [['batch-serial',serial],['batch-parallel',parallel]];
    for(const [condition,w] of arms) await measure(condition,manifest.batch,w,i);
  }
} finally { serial.close(); parallel.close(); }
const groups = new Map();
for(const s of result.samples) { const key = s.condition+':'+(s.paths.length === 1 ? s.paths[0] : 'batch'); if(!groups.has(key)) groups.set(key,[]);groups.get(key).push(s); }
result.summary = [...groups].map(([condition, samples])=>{
  const times = samples.filter(s=>!s.error).map(s=>s.ms).sort((a,b)=>a-b);
  return { condition, attempts:samples.length, successes:times.length, medianMs:times.length?(times[Math.floor((times.length-1)/2)]+times[Math.floor(times.length/2)])/2:null,minMs:times[0]??null,maxMs:times.at(-1)??null };
});
console.log(JSON.stringify(result,null,2));
