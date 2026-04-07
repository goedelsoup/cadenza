<script lang="ts">
  import { onMount, onDestroy } from 'svelte'
  import { defaultSession, withPhrase, buildContext, estimateTokens } from '$session'
  import { generatePhrase, generateLyrics }  from '$api/compose'
  import { buildMidiBytes, triggerDownload } from '$api/export'
  import { wasmIngestPhrase, wasmAttachLyrics, wasmSetKey, wasmSetTempo, wasmSetTimeSig, wasmPhraseToMidi, wasmPhraseToMusicXml } from '$api/wasm-bridge'
  import { daemon, type DaemonStatus } from '$api/daemon-bridge'
  import { CadenzaPlayer } from '$player'
  import type { SessionPhrase, StyleTag, GenerationTarget } from '$types'

  let session = defaultSession()
  let prompt  = ''
  let loading = false
  let logs: { ts: string; msg: string; cls: string }[] = []

  // Playback state — player is constructed eagerly but Tone.start() (which
  // creates the AudioContext) is deferred until the first play click so we
  // satisfy the browser's autoplay policy.
  const player          = new CadenzaPlayer('piano')
  let playerReady       = false
  let playingId: number | null = null
  let beatTick          = 0

  // Daemon connection status — surfaces in the header. Subscription is set
  // up in onMount and torn down in onDestroy.
  let daemonStatus: DaemonStatus = 'disconnected'
  let unsubDaemon: (() => void) | null = null

  player.on('start', () => { /* no-op, playingId set in handler */ })
  player.on('stop',  () => { playingId = null; beatTick = 0 })
  player.on('beat',  () => { beatTick += 1 })

  const MODES: string[]             = ['dorian','natural minor','harmonic minor','major','lydian','mixolydian','phrygian','locrian']
  const ROOTS: string[]             = ['C','C#','D','D#','E','F','F#','G','G#','A','A#','B']
  const STYLE_TAGS: StyleTag[]      = ['jazz','blues','modal','latin','bossa','funk','classical','folk','ambient','cinematic']
  const TARGETS: GenerationTarget[] = ['chord progression','melody phrase','bass line','full arrangement','motif variation','lyric']

  function log(msg: string, cls = '') {
    const now = new Date()
    const ts  = `${String(now.getMinutes()).padStart(2,'0')}:${String(now.getSeconds()).padStart(2,'0')}`
    logs = [...logs, { ts, msg, cls }]
  }

  $: ctx     = buildContext(session)
  $: ctxToks = estimateTokens(ctx)

  function toggleStyle(tag: StyleTag) {
    session = {
      ...session,
      styles: session.styles.includes(tag)
        ? session.styles.filter(s => s !== tag)
        : [...session.styles, tag],
    }
    log(`style → [${session.styles.join(', ')}]`, 'ctx')
  }

  async function compose() {
    if (!prompt.trim()) { log('no prompt', 'warn'); return }
    loading = true
    log(`composing "${session.target}" — ctx ~${ctxToks} tok`, 'info')
    try {
      const phrase = await generatePhrase(prompt, ctx, session.target)
      const summary = await wasmIngestPhrase(JSON.stringify(phrase))
      const sp: SessionPhrase = {
        id:        summary?.id ?? Date.now(),
        label:     phrase.type,
        bars:      phrase.bars   ?? session.bars,
        tempo:     phrase.tempo  ?? session.tempo,
        noteCount: phrase.notes?.length ?? 0,
        warnings:  summary ? (JSON.parse(summary.warnings_json) as string[]) : [],
        raw:       phrase,
      }
      session = withPhrase(session, sp)
      log(`phrase #${sp.id}: ${sp.noteCount} notes · ${sp.bars} bars`, 'ok')
      for (const w of sp.warnings) log(w, 'warn')
    } catch (e) {
      log(`error: ${e}`, 'err')
    }
    loading = false
    prompt  = ''
  }

  async function exportMidi(sp: SessionPhrase) {
    const wasmBytes = await wasmPhraseToMidi(sp.id)
    const bytes     = wasmBytes ?? buildMidiBytes(sp.raw, sp.tempo)
    triggerDownload(bytes, `cadenza_${sp.label.replace(/\s+/g,'_')}_${sp.id}.mid`)
    log(`MIDI exported: ${bytes.byteLength} bytes`, 'ok')
  }

  async function togglePlay(sp: SessionPhrase) {
    try {
      if (!playerReady) {
        await player.load()
        playerReady = true
        log('audio engine ready', 'ok')
      }
      if (playingId === sp.id) {
        player.stop()
        return
      }
      player.play(sp)
      playingId = sp.id
      log(`▶ playing #${sp.id}`, 'info')
    } catch (e) {
      log(`playback error: ${e}`, 'err')
    }
  }

  async function composeLyric(sp: SessionPhrase) {
    if (sp.noteCount === 0) { log('cannot lyric an empty phrase', 'warn'); return }
    const userPrompt = prompt.trim() || `Write a lyric for phrase #${sp.id}`
    loading = true
    log(`writing lyric for #${sp.id} (${sp.noteCount} syllables)`, 'info')
    try {
      const { line, warnings } = await generateLyrics(userPrompt, ctx, sp)
      // Marshal to the schema attach_lyrics expects: { syllables: [{text,wordIndex,stress}], raw }
      const payload = JSON.stringify({
        syllables: line.syllables.map(s => ({ text: s.text, wordIndex: s.wordIndex, stress: s.stress })),
        raw:       line.rawText,
      })
      await wasmAttachLyrics(sp.id, payload)
      // Re-render the card by replacing this phrase in session.phrases.
      session = {
        ...session,
        phrases: session.phrases.map(p =>
          p.id === sp.id ? { ...p, lyrics: [...(p.lyrics ?? []), line] } : p
        ),
      }
      log(`lyric: "${line.rawText}"`, 'ok')
      for (const w of warnings) log(w, 'warn')
    } catch (e) {
      log(`lyric error: ${e}`, 'err')
    }
    loading = false
    prompt  = ''
  }

  async function exportMusicXml(sp: SessionPhrase) {
    const xml = await wasmPhraseToMusicXml(sp.id, sp.label, 'Cadenza')
    if (xml) {
      triggerDownload(new TextEncoder().encode(xml), `cadenza_${sp.id}.musicxml`, 'application/xml')
      log('MusicXML exported', 'ok')
    } else {
      log('MusicXML requires WASM — run: nx run cadenza-wasm:wasm:build', 'warn')
    }
  }

  function handleKey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') compose()
  }

  onMount(async () => {
    await wasmSetKey(session.root, session.mode)
    await wasmSetTempo(session.tempo)
    await wasmSetTimeSig(session.timeSignature)
    log('cadenza ready', 'ok')
    log(`key: ${session.root} ${session.mode} · ${session.tempo}bpm · ${session.timeSignature}`, 'ctx')

    // Hook up the native daemon status indicator. The bridge degrades
    // gracefully — playback will fall back to Tone.js if the daemon is
    // not running.
    const bridge = daemon()
    daemonStatus = bridge.getStatus()
    unsubDaemon  = bridge.onStatusChange((s) => {
      daemonStatus = s
      if      (s === 'connected')    log('daemon connected', 'ok')
      else if (s === 'disconnected') log('daemon disconnected — start with: mise run daemon', 'warn')
    })
    void bridge.connect()
  })

  onDestroy(() => {
    unsubDaemon?.()
  })
</script>

<div class="app">
  <header>
    <span class="logo">caden<em>za</em></span>
    <span class="badge">v0.1</span>
    <div class="daemon-pill" class:connected={daemonStatus === 'connected'} class:connecting={daemonStatus === 'connecting'}
         title={daemonStatus === 'connected' ? 'native daemon connected — low-latency playback' : 'daemon offline — playing through Tone.js. Start with: mise run daemon'}>
      <span class="dot"></span>
      daemon: {daemonStatus}
    </div>
    <div class="ctx-pill">
      <span class="dot"></span>
      context: ~{ctxToks} tok
    </div>
  </header>

  <aside>
    <section>
      <label class="group-label">key &amp; mode</label>
      <div class="row">
        <select bind:value={session.root} on:change={() => { wasmSetKey(session.root, session.mode); log(`root → ${session.root}`, 'ctx') }}>
          {#each ROOTS as r}<option>{r}</option>{/each}
        </select>
        <select bind:value={session.mode} on:change={() => { wasmSetKey(session.root, session.mode); log(`mode → ${session.mode}`, 'ctx') }}>
          {#each MODES as m}<option>{m}</option>{/each}
        </select>
      </div>
    </section>

    <section>
      <label class="group-label">tempo</label>
      <div class="slider-row">
        <input type="range" min="40" max="240" step="1" bind:value={session.tempo}
          on:change={() => wasmSetTempo(session.tempo)} />
        <span>{session.tempo} bpm</span>
      </div>
    </section>

    <section>
      <label class="group-label">time signature</label>
      <select bind:value={session.timeSignature} on:change={() => wasmSetTimeSig(session.timeSignature)}>
        {#each ['4/4','3/4','6/8','5/4','7/8','12/8'] as ts}<option>{ts}</option>{/each}
      </select>
    </section>

    <section>
      <label class="group-label">style</label>
      <div class="tags">
        {#each STYLE_TAGS as tag}
          <button class="tag" class:active={session.styles.includes(tag)} on:click={() => toggleStyle(tag)}>{tag}</button>
        {/each}
      </div>
    </section>

    <section>
      <label class="group-label">generate</label>
      <select bind:value={session.target}>
        {#each TARGETS as t}<option>{t}</option>{/each}
      </select>
      <div class="slider-row" style="margin-top:8px">
        <input type="range" min="2" max="32" step="2" bind:value={session.bars} />
        <span>{session.bars} bars</span>
      </div>
    </section>
  </aside>

  <main>
    <div class="prompt-bar">
      <textarea
        bind:value={prompt}
        placeholder="describe what you want… (⌘↵ to compose)"
        rows="3"
        on:keydown={handleKey}
      ></textarea>
      <button class="btn-compose" on:click={compose} disabled={loading}>
        {#if loading}<span class="spinner"></span>{/if}
        {loading ? 'composing…' : 'compose'}
      </button>
    </div>

    <div class="phrases">
      {#if session.phrases.length === 0}
        <div class="empty">♩ set parameters, then describe what you want</div>
      {/if}
      {#each session.phrases as sp (sp.id)}
        <div class="phrase-card" class:playing={playingId === sp.id}>
          <div class="phrase-header">
            <span class="phrase-label">{sp.label}</span>
            <span class="phrase-meta">{sp.bars} bars · {sp.tempo} bpm</span>
          </div>

          {#if sp.raw.chords?.length}
            <div class="chord-row">
              {#each sp.raw.chords as ch, i}
                <span class="chord" class:tonic={i===0}>{ch}</span>
              {/each}
            </div>
          {/if}

          {#if sp.raw.notes?.length}
            <div class="piano-roll">
              {#each (() => {
                const notes = sp.raw.notes ?? []
                const minP  = Math.min(...notes.map(n => n.pitch))
                const maxP  = Math.max(...notes.map(n => n.pitch))
                const range = Math.max(maxP - minP + 2, 12)
                const total = Math.max(...notes.map(n => n.start + n.dur))
                return notes.map(n => ({
                  x: (n.start / total) * 100,
                  w: (n.dur   / total) * 100,
                  y: ((maxP + 1 - n.pitch) / range) * 100,
                  h: (1 / range) * 85,
                }))
              })() as block}
                <div class="note"
                  style="left:{block.x}%;width:{block.w}%;top:{block.y}%;height:{block.h}%">
                </div>
              {/each}
              {#if playingId === sp.id}
                {@const beatsPerBar = parseInt((sp.raw.time_signature ?? '4/4').split('/')[0], 10) || 4}
                {@const totalBeats  = beatsPerBar * sp.bars}
                <div class="playhead" style="left:{Math.min(100, ((beatTick - 1) / totalBeats) * 100)}%"></div>
              {/if}
            </div>
          {/if}

          {#if sp.lyrics?.length}
            <div class="lyric-line">{sp.lyrics[0].rawText}</div>
          {/if}

          {#if sp.warnings.length}
            <div class="warnings">{sp.warnings.join(' · ')}</div>
          {/if}

          <div class="phrase-actions">
            <button class="play" on:click={() => togglePlay(sp)}>
              {playingId === sp.id ? '■ stop' : '▶ play'}
            </button>
            <button on:click={() => {
              prompt  = `Variation of phrase #${sp.id} (${sp.label}): `
              session = { ...session, target: 'motif variation' }
            }}>variation</button>
            <button on:click={() => composeLyric(sp)} disabled={loading}>lyric</button>
            <button class="export" on:click={() => exportMidi(sp)}>export MIDI</button>
            <button on:click={() => exportMusicXml(sp)}>MusicXML</button>
          </div>
        </div>
      {/each}
    </div>
  </main>

  <footer>
    {#each logs as entry}
      <div class="log-line">
        <span class="ts">{entry.ts}</span>
        <span class={entry.cls}>{entry.msg}</span>
      </div>
    {/each}
  </footer>
</div>

<style>
  *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
  .app {
    display: grid;
    grid-template-columns: 260px 1fr;
    grid-template-rows: 44px 1fr 180px;
    grid-template-areas: "top top" "side main" "foot foot";
    height: 100vh;
    font-family: var(--font-mono, monospace);
    background: var(--color-background-primary);
    color: var(--color-text-primary);
    border: 0.5px solid var(--color-border-tertiary);
    border-radius: var(--border-radius-lg);
    overflow: hidden;
  }
  header { grid-area: top; display:flex; align-items:center; gap:12px; padding:0 16px; border-bottom:0.5px solid var(--color-border-tertiary); background:var(--color-background-secondary); }
  .logo { font-size:15px; font-weight:500; letter-spacing:-0.3px; }
  .logo em { font-style:normal; color:#1D9E75; }
  .badge { font-size:10px; padding:2px 8px; border:0.5px solid var(--color-border-secondary); border-radius:99px; color:var(--color-text-secondary); }
  .ctx-pill { display:flex; align-items:center; gap:6px; font-size:11px; color:var(--color-text-secondary); }
  .dot { width:6px; height:6px; border-radius:50%; background:#1D9E75; }
  .daemon-pill { margin-left:auto; display:flex; align-items:center; gap:6px; font-size:11px; color:var(--color-text-secondary); padding:2px 8px; border:0.5px solid var(--color-border-tertiary); border-radius:99px; cursor:help; }
  .daemon-pill .dot { background:#999; }
  .daemon-pill.connecting .dot { background:#BA7517; }
  .daemon-pill.connected  .dot { background:#1D9E75; }
  .daemon-pill.connected  { color:#0F6E56; border-color:#1D9E75; }
  aside { grid-area:side; border-right:0.5px solid var(--color-border-tertiary); overflow-y:auto; padding:12px; display:flex; flex-direction:column; gap:14px; }
  .group-label { display:block; font-size:10px; text-transform:uppercase; letter-spacing:1px; color:var(--color-text-secondary); margin-bottom:6px; }
  .row { display:flex; gap:6px; }
  .row select { flex:1; }
  .slider-row { display:flex; align-items:center; gap:10px; }
  .slider-row input { flex:1; }
  .slider-row span { font-size:12px; font-weight:500; min-width:60px; text-align:right; }
  select { width:100%; font-size:12px; }
  .tags { display:flex; flex-wrap:wrap; gap:4px; }
  .tag { font-size:11px; padding:2px 8px; border-radius:99px; border:0.5px solid var(--color-border-secondary); background:transparent; color:var(--color-text-secondary); cursor:pointer; }
  .tag.active { background:#E1F5EE; border-color:#1D9E75; color:#0F6E56; }
  main { grid-area:main; display:flex; flex-direction:column; overflow:hidden; }
  .prompt-bar { padding:12px 16px; border-bottom:0.5px solid var(--color-border-tertiary); display:flex; gap:8px; align-items:flex-end; }
  .prompt-bar textarea { flex:1; resize:none; font-size:13px; font-family:var(--font-sans); padding:8px 10px; border-radius:var(--border-radius-md); }
  .btn-compose { height:36px; padding:0 16px; font-size:12px; font-weight:500; background:#1D9E75; color:white; border:none; border-radius:var(--border-radius-md); cursor:pointer; display:flex; align-items:center; gap:6px; white-space:nowrap; }
  .btn-compose:hover { background:#0F6E56; }
  .btn-compose:disabled { opacity:.5; cursor:not-allowed; }
  .phrases { flex:1; overflow-y:auto; padding:14px 16px; }
  .empty { display:flex; align-items:center; justify-content:center; height:100%; font-size:13px; color:var(--color-text-secondary); font-family:var(--font-sans); }
  .phrase-card { border:0.5px solid var(--color-border-tertiary); border-radius:var(--border-radius-md); padding:12px 14px; margin-bottom:10px; background:var(--color-background-secondary); transition:border-color .15s, box-shadow .15s; }
  .phrase-card.playing { border-color:#1D9E75; box-shadow:0 0 0 1px #1D9E75; }
  .playhead { position:absolute; top:0; bottom:0; width:1px; background:#E24B4A; pointer-events:none; }
  .phrase-actions .play { border-color:#1D9E75; color:#0F6E56; font-weight:500; }
  .phrase-header { display:flex; justify-content:space-between; align-items:center; margin-bottom:8px; }
  .phrase-label { font-size:12px; font-weight:500; }
  .phrase-meta { font-size:10px; color:var(--color-text-secondary); }
  .chord-row { display:flex; flex-wrap:wrap; gap:4px; margin-bottom:8px; }
  .chord { font-size:11px; padding:2px 8px; border-radius:4px; border:0.5px solid var(--color-border-secondary); font-family:var(--font-mono); }
  .chord.tonic { border-color:#1D9E75; color:#0F6E56; }
  .piano-roll { width:100%; height:56px; background:var(--color-background-primary); border:0.5px solid var(--color-border-tertiary); border-radius:4px; position:relative; overflow:hidden; margin-bottom:8px; }
  .note { position:absolute; border-radius:2px; background:#1D9E75; opacity:.85; }
  .lyric-line { font-size:12px; font-style:italic; color:var(--color-text-secondary); margin-bottom:6px; padding:4px 6px; border-left:2px solid #1D9E75; background:var(--color-background-primary); border-radius:2px; }
  .warnings { font-size:10px; color:#BA7517; margin-bottom:6px; }
  .phrase-actions { display:flex; gap:6px; }
  .phrase-actions button { font-size:11px; padding:3px 10px; border-radius:var(--border-radius-md); cursor:pointer; border:0.5px solid var(--color-border-secondary); background:transparent; color:var(--color-text-secondary); }
  .phrase-actions button:hover { border-color:#1D9E75; color:#0F6E56; }
  .phrase-actions .export { border-color:#1D9E75; color:#0F6E56; font-weight:500; }
  footer { grid-area:foot; border-top:0.5px solid var(--color-border-tertiary); background:var(--color-background-secondary); overflow-y:auto; padding:8px 12px; font-size:11px; font-family:var(--font-mono); color:var(--color-text-secondary); }
  .log-line { line-height:1.7; }
  .log-line .ts { opacity:.4; margin-right:8px; }
  .log-line .ok   { color:#1D9E75; }
  .log-line .info { color:#378ADD; }
  .log-line .warn { color:#BA7517; }
  .log-line .err  { color:#E24B4A; }
  .log-line .ctx  { color:#7F77DD; }
  .spinner { width:10px; height:10px; border-radius:50%; border:1.5px solid rgba(255,255,255,.3); border-top-color:white; animation:spin .6s linear infinite; }
  @keyframes spin { to { transform:rotate(360deg); } }
</style>
