# pi-source-extractor

A Rust C++ source extractor for Pi and other coding agents. It identifies complete function, constructor, operator and template ranges, preserving one-based inclusive source lines and explicit parser diagnostics.

Tree-sitter supplies syntax parsing; this project supplies extraction policy and a persistent JSONL worker. It does not use an LLM or need API credentials. It is a syntax extractor, not a C++ compiler, semantic dependency resolver or guarantee of complete C++26 coverage.

## Build and test

Install a current stable Rust toolchain. The JavaScript client requires Node.js 22 or newer and has no npm dependencies.

```sh
cargo build --release --locked
cargo test --release --locked
npm test
```

The executable is `target/release/pi-source-extractor` (`.exe` on Windows). Keep `Cargo.lock` for reproducible dependency versions. The Node client uses that path by default; pass `binary` to use an installed executable elsewhere.

## Node / Pi integration

```js
import { Extractor, materialize } from './js/index.mjs';

const extractor = new Extractor({ threads: 4 });
try {
  const source = 'int answer() { return 42; }';
  const response = await extractor.extract([{ path: 'answer.cpp', source }]);
  for (const result of response.results) {
    if (result.error) throw new Error(result.error);
    console.log(materialize(result.path, source, result.extraction));
  }
} finally {
  extractor.close();
}
```

Create one client for a Pi extension session, send selected files in batches, and call `close()` from the extension's session shutdown handler. This is a reusable client, not an automatically registered Pi extension. The worker receives source strings and never opens the supplied path labels. File selection, reading, helper selection and evidence formatting belong to the caller. Keep a request-local extraction cache if helpers need the same file again; invalidate it between source revisions.

The client starts the worker lazily, correlates concurrent requests, and bounds pending work. An idle worker does not keep Node running. A timeout or worker failure rejects every pending request without retrying; a later explicit call can start a new worker. Pass `{ signal }` as the second argument to `extract()` to support cancellation. Aborting any request terminates the shared worker and rejects all pending requests. `close()` is final. A worker process isolates native parser failures from the agent process; it does not guarantee native dependencies are crash-free.

## Parallelism

Each parser is owned by one thread and reused. Single files and batches below 64 KiB of combined source are processed serially. Larger multi-file batches use an ordered Rayon parallel iterator. The pool is created lazily; default concurrency is available CPUs capped at four, configurable from one to 32. Parallelism is across files, not within an individual parse. Output order always matches input order. Small-batch threshold and default thread count are conservative policies, not universal optimums.

## Protocol

```sh
printf '%s\n' '{"id":1,"files":[{"path":"example.cpp","source":"int example() { return 1; }"}]}' \
  | target/release/pi-source-extractor --stdio --threads 4
```

Each request is one JSON line with an integer `id` and `files: [{path, source}]`. Each response has the same `id`, ordered `results`, and `parallel`. A successful file result contains `path` and `extraction`:

```json
{
  "units": [{"name": "example", "line": 1, "end": 1}],
  "parseHasError": false,
  "omitted": [],
  "recovered": []
}
```

File failures contain `error` instead of `extraction`. Request errors contain top-level `error`; malformed JSON has `id: null`. Syntax errors are diagnostic data, not transport errors. Functions with erroneous bodies are omitted. A narrow recovery accepts a known Tree-sitter default-argument `= {}` signature error only when all errors match that condition and the body parses cleanly. Deleted/defaulted methods without bodies are recorded as omitted. Nested functions/constructs retain preorder. Whole-line ranges can include neighboring text on the same line.

Limits: 64 files per request, 4 MiB per source, 16 MiB per JSON request including escaping and its newline, 4,096 bytes per path label. The CLI closes on oversized request framing. The Node client additionally caps a response buffer at 32 MiB and defaults to eight outstanding requests and a 10-second deadline per request (including queue time). Errors and diagnostics must not be silently treated as complete evidence.

## Benchmarks

`node benchmarks/run.mjs manifest.json` emits JSON timing samples. A local manifest names three `singles` files and eight `batch` files. Paths may point to any locally available C++ sources. No source content is emitted. All measured attempts, errors, startup observations and output hashes are retained; successful latency and failure counts are reported separately. The benchmark makes no network or model calls.

It measures cold worker startup, persistent single-file extraction, one-thread/four-thread batch throughput and repeated extraction of the largest single input. It includes source reading and materialization but excludes output hashing. Filesystem caches are warm; this is not a cold-disk benchmark. Compare output equivalence as well as speed. Private inputs and their measurements should stay outside this repository.

## License

MIT. Tree-sitter and its C++ grammar retain their own upstream licenses.
