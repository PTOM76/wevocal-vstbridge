// ブリッジの成果物（wasm、JS のグルー、worklet 用に加工したグルー）を、wevocal-lib の web/src/plugins/vst/ に書き出す。
// 使い方: ./build-wasm.sh のあとに node scripts/export-to-lib.mjs <wevocal-lib のフォルダー>
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { workletGlue } from './make-package.mjs'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const lib = process.argv[2]
if (!lib) {
  console.error('使い方: node scripts/export-to-lib.mjs <wevocal-lib のフォルダー>')
  process.exit(2)
}
const out = join(lib, 'web/src/plugins/vst')
mkdirSync(out, { recursive: true })
for (const f of ['vstbridge.js', 'vstbridge.d.ts', 'vstbridge_bg.wasm']) copyFileSync(join(root, 'pkg-web', f), join(out, f))

// worklet 用のグルーは、processor の頭に埋め込む文字列として TS の定数にする
const glue = workletGlue()
writeFileSync(
  join(out, 'workletGlue.ts'),
  `// 自動生成（wevocal-vstbridge の scripts/export-to-lib.mjs）。processor の頭に埋め込む、AudioWorklet 用に加工したグルー。手で直さない\nexport const WORKLET_GLUE = ${JSON.stringify(glue)}\n`,
)
console.log(`${out} に書き出した（wasm ${(readFileSync(join(root, 'pkg-web/vstbridge_bg.wasm')).length / 1024) | 0} KB）`)
