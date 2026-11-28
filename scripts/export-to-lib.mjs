// ブリッジの成果物（wasm、JS のグルー、worklet 用に加工したグルー）を web/ にそろえ、web/ の中身を wevocal-lib の web/src/plugins/vst/ に書き出す。
// web/ がパッケージ（wevocal-vstbridge）の中身で、processor、写し、画面の TS もここが正本（wevocal-lib の側では直さない）
// 使い方: ./build-wasm.sh のあとに node scripts/export-to-lib.mjs [wevocal-lib のフォルダー]（省くと web/ をそろえるだけ）
// node scripts/export-to-lib.mjs ../wevocalsynth/wevocal-lib/
import { copyFileSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { workletGlue } from './make-package.mjs'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const web = join(root, 'web')
for (const f of ['vstbridge.js', 'vstbridge.d.ts', 'vstbridge_bg.wasm']) copyFileSync(join(root, 'pkg-web', f), join(web, f))

// worklet 用のグルーは、processor の頭に埋め込む文字列として TS の定数にする（wasm を作り直したら必ず作り直す。古いと worklet で wasm の import が合わない）
writeFileSync(
  join(web, 'workletGlue.ts'),
  `// 自動生成（wevocal-vstbridge の scripts/export-to-lib.mjs）。processor の頭に埋め込む、AudioWorklet 用に加工したグルー。手で直さない\nexport const WORKLET_GLUE = ${JSON.stringify(workletGlue())}\n`,
)

const lib = process.argv[2]
if (lib) {
  const out = join(lib, 'web/src/plugins/vst')
  mkdirSync(out, { recursive: true })
  // パッケージの入口（api.ts）は wevocal-lib では使わない（lib は個々のファイルを読む）
  for (const f of readdirSync(web)) if (f !== 'api.ts') copyFileSync(join(web, f), join(out, f))
  console.log(`${out} に書き出した（wasm ${(readFileSync(join(web, 'vstbridge_bg.wasm')).length / 1024) | 0} KB）`)
} else console.log('web/ をそろえた')
