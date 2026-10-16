// tests/browser/ を配信する（fetch は file:// では CORS に引っかかるため）。使い方: node tests/browser/serve.mjs [ポート（既定 8991）]
import { createServer } from 'node:http'
import { readFile } from 'node:fs/promises'
import { extname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('.', import.meta.url))
const port = Number(process.argv[2] ?? 8991)
const types = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm', '.dll': 'application/octet-stream' }

createServer(async (req, res) => {
  const p = decodeURIComponent(req.url.split('?')[0])
  const file = join(root, p === '/' ? '/index.html' : p)
  try {
    const data = await readFile(file)
    res.writeHead(200, { 'Content-Type': types[extname(file)] ?? 'application/octet-stream' })
    res.end(data)
  } catch {
    res.writeHead(404)
    res.end(`not found: ${p}`)
  }
}).listen(port, () => console.log(`http://localhost:${port}/`))
