#!/usr/bin/env node
// Cross-platform npm entry point. The voice engine remains Python.
const { spawnSync } = require('node:child_process');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const env = {
  ...process.env,
  GHOSTLESS_SUITE: root,
  GHOSTLESS_LAUNCHER: 'npm',
  PYTHONUTF8: '1',
  PYTHONIOENCODING: 'utf-8',
  PYTHONPATH: [path.join(root, 'lib'), process.env.PYTHONPATH].filter(Boolean).join(path.delimiter),
};

const candidates = process.env.GHOSTLESS_PYTHON
  ? [[process.env.GHOSTLESS_PYTHON, []]]
  : process.platform === 'win32'
    ? [['py', ['-3']], ['python', []], ['python3', []]]
    : [['python3', []], ['python', []]];

for (const [program, prefix] of candidates) {
  const probe = spawnSync(program, [...prefix, '-c', 'import sys; sys.exit(sys.version_info < (3, 11))'], {
    env,
    stdio: 'ignore',
  });
  if (probe.error || probe.status !== 0) continue;

  const child = spawnSync(program, [...prefix, '-m', 'ghostless', ...process.argv.slice(2)], {
    env,
    stdio: 'inherit',
  });
  if (child.error) {
    console.error(`ghostless: could not start Python: ${child.error.message}`);
    process.exit(1);
  }
  if (child.signal) {
    console.error(`ghostless: Python stopped on signal ${child.signal}`);
    process.exit(1);
  }
  process.exit(child.status ?? 1);
}

console.error('ghostless: Python 3.11 or newer is required. Set GHOSTLESS_PYTHON to its executable path.');
process.exit(1);
