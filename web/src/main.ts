// Entry point — import the self-contained Cadenza app
// The prototype UI lives in index.html for now (served via Vite)
// This will wire up the WASM bridge on load

import { setKey, setTempo, setTimeSig } from './api/wasm-bridge'

// Sync WASM session with initial defaults
setKey('D', 'dorian')
setTempo(120)
setTimeSig('4/4')

console.log('[cadenza] initialized')
