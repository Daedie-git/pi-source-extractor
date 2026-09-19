import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const DEFAULT_BINARY = fileURLToPath(new URL(`../target/release/pi-source-extractor${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const MAX_REQUEST_BYTES = 16 * 1024 * 1024;
const MAX_RESPONSE_BYTES = 32 * 1024 * 1024;

/** One persistent worker; batches run in parallel inside Rust when useful. */
export class Extractor {
  #child;
  #pending = new Map();
  #nextId = 1;
  #closed = false;
  #buffer = '';
  #stderr = '';

  constructor({ binary = DEFAULT_BINARY, threads, timeoutMs = 10000, maxPending = 8 } = {}) {
    if (threads !== undefined && (!Number.isInteger(threads) || threads < 1 || threads > 32)) throw new Error('threads must be 1..32');
    if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new Error('timeoutMs must be positive');
    if (!Number.isInteger(maxPending) || maxPending < 1 || maxPending > 64) throw new Error('maxPending must be 1..64');
    this.binary = binary;
    this.threads = threads;
    this.timeoutMs = timeoutMs;
    this.maxPending = maxPending;
  }

  #start() {
    const args = ['--stdio'];
    if (this.threads !== undefined) args.push('--threads', String(this.threads));
    const child = spawn(this.binary, args, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    this.#child = child;
    this.#buffer = '';
    this.#stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', chunk => { if (this.#child === child) this.#stderr = (this.#stderr + chunk).slice(-8192); });
    child.stdout.on('data', chunk => {
      if (this.#child !== child) return;
      this.#buffer += chunk;
      if (Buffer.byteLength(this.#buffer) > MAX_RESPONSE_BYTES) return this.#fail(child, new Error('extractor response exceeds limit'));
      for (;;) {
        const end = this.#buffer.indexOf('\n');
        if (end < 0) break;
        const line = this.#buffer.slice(0, end);
        this.#buffer = this.#buffer.slice(end + 1);
        let response;
        try { response = JSON.parse(line); } catch { return this.#fail(child, new Error('invalid extractor JSON')); }
        if (!response || typeof response !== 'object' || Array.isArray(response)) return this.#fail(child, new Error('invalid extractor response object'));
        const pending = this.#pending.get(response.id);
        if (!pending) return this.#fail(child, new Error(response.error || 'unexpected extractor response id'));
        if (!Array.isArray(response.results)) return this.#fail(child, new Error('invalid extractor results'));
        this.#pending.delete(response.id);
        pending.cleanup();
        if (response.error) pending.reject(new Error(response.error));
        else pending.resolve(response);
      }
      if (this.#pending.size === 0) this.#refs(false);
    });
    child.on('error', error => this.#fail(child, new Error(`Cannot start extractor: ${error.message}. Build with cargo build --release --locked.`)));
    child.on('exit', (code, signal) => this.#fail(child, new Error(`extractor exited (${signal ?? code})${this.#stderr ? ': '+this.#stderr : ''}`)));
    child.stdin.on('error', error => this.#fail(child, error));
  }

  #refs(active) {
    if (!this.#child) return;
    const method = active ? 'ref' : 'unref';
    this.#child[method]();
    for (const stream of [this.#child.stdin, this.#child.stdout, this.#child.stderr]) stream[method]?.();
  }

  #fail(child, error) {
    if (this.#child !== child) return;
    this.#child = undefined;
    child.kill();
    child.stdin.destroy(); child.stdout.destroy(); child.stderr.destroy();
    for (const pending of this.#pending.values()) { pending.cleanup(); pending.reject(error); }
    this.#pending.clear();
    this.#buffer = '';
  }

  /** Returns ordered per-file extraction results; a file error is explicit. No automatic retries. */
  extract(files, { signal } = {}) {
    if (this.#closed) return Promise.reject(new Error('extractor is closed'));
    if (signal?.aborted) return Promise.reject(signal.reason ?? new Error('extraction aborted'));
    if (!Array.isArray(files) || files.length > 64 || files.some(f => typeof f?.path !== 'string' || typeof f?.source !== 'string')) return Promise.reject(new Error('expected at most 64 {path, source} files'));
    if (this.#pending.size >= this.maxPending) return Promise.reject(new Error('extractor queue is full'));
    const id = this.#nextId++;
    const line = JSON.stringify({ id, files }) + '\n';
    if (Buffer.byteLength(line) > MAX_REQUEST_BYTES) return Promise.reject(new Error('request exceeds 16 MiB'));
    if (!this.#child) this.#start();
    this.#refs(true);
    const child = this.#child;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => this.#fail(child, new Error('extractor request timed out')), this.timeoutMs);
      const abort = () => this.#fail(child, signal.reason ?? new Error('extraction aborted'));
      const cleanup = () => { clearTimeout(timer); signal?.removeEventListener('abort', abort); };
      this.#pending.set(id, { resolve, reject, cleanup });
      signal?.addEventListener('abort', abort, { once: true });
      child.stdin.write(line, error => { if (error) this.#fail(child, error); });
    });
  }

  /** Shutdown also rejects in-flight requests. An idle worker never keeps Node alive. */
  close() {
    this.#closed = true;
    if (this.#child) this.#fail(this.#child, new Error('extractor closed'));
  }
}

/** Materialize source text using the same inclusive, one-based line contract as extraction. */
export function materialize(path, source, extraction) {
  const lines = source.split('\n');
  return extraction.units.map(unit => {
    if (!Number.isInteger(unit.line) || !Number.isInteger(unit.end) || unit.line < 1 || unit.end < unit.line || unit.end > lines.length) throw new Error('invalid extraction source range');
    return { path, ...unit, text: lines.slice(unit.line - 1, unit.end).join('\n') };
  });
}
