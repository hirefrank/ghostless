#!/usr/bin/env node
// Run the Python suite with a portable interpreter selection.
const { spawnSync } = require('node:child_process');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const env = {
  ...process.env,
  GHOSTLESS_NO_UPDATE: '1',
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
  const result = spawnSync(program, [...prefix, '-m', 'unittest', 'discover', '-s', path.join(root, 'tests'), '-p', 'test_*.py'], {
    cwd: root,
    env,
    stdio: 'inherit',
  });
  process.exit(result.status ?? 1);
}

console.error('ghostless tests require Python 3.11 or newer.');
process.exit(1);
