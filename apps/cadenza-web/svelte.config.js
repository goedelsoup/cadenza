import adapter from '@sveltejs/adapter-static'
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte'

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({ fallback: 'index.html' }),
    alias: {
      '$types':   '../../packages/cadenza-types/src',
      '$session': '../../packages/cadenza-session/src',
      '$api':     '../../packages/cadenza-api/src',
      '$player':  '../../packages/cadenza-player/src',
    }
  }
}

export default config
